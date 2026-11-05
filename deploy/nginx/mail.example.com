# /etc/nginx/sites-available/mail.meowlabs.id — IRIS web + API (behind Cloudflare)
server {
    server_name mail.meowlabs.id;

    # Real client IP from Cloudflare only (not spoofable by hitting the origin directly).
    include /etc/nginx/snippets/cloudflare-realip.conf;
    include /etc/nginx/snippets/iris-headers.conf;

    # API + auth (same origin for CSRF cookie). The API trusts X-Real-IP only from loopback.
    location /api/ {
        proxy_pass http://127.0.0.1:8000;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    location /auth/ {
        proxy_pass http://127.0.0.1:8000;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    location = /health { proxy_pass http://127.0.0.1:8000/health; }
    location = /ready  { proxy_pass http://127.0.0.1:8000/ready; }

    # Static frontend. The app uses hash routes (#/temp), so no SPA fallback is needed:
    # unknown paths get the real 404 page. API 404s are not intercepted.
    root /srv/iris/dist;
    index index.html;
    error_page 404 /404.html;

    # Hashed build files never change.
    location /assets/ {
        include /etc/nginx/snippets/iris-headers.conf;
        add_header Cache-Control "public, max-age=31536000, immutable" always;
        try_files $uri =404;
    }

    # HTML and root files (favicon, og-image, robots, sitemap): always revalidate.
    location / {
        include /etc/nginx/snippets/iris-headers.conf;
        add_header Cache-Control "no-cache" always;
        try_files $uri $uri/ =404;
    }

    listen 443 ssl;
    ssl_certificate /etc/letsencrypt/live/mail.meowlabs.id/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/mail.meowlabs.id/privkey.pem;
    include /etc/letsencrypt/options-ssl-nginx.conf;
    ssl_dhparam /etc/letsencrypt/ssl-dhparams.pem;
}

server {
    if ($host = mail.meowlabs.id) {
        return 301 https://$host$request_uri;
    }
    server_name mail.meowlabs.id;
    listen 80;
    return 404;
}
