// Builds the in-app integration docs page from docs/INTEGRATIONS.md.
// The markdown file stays the source of truth; this script converts a small,
// well-defined subset (ATX headings, paragraphs, fenced code, ordered and
// unordered lists, tables, inline code, bold, links, horizontal rules) to
// sanitized HTML and writes it as a TypeScript module imported by main.ts.
// Run by the prebuild/pretypecheck npm scripts, and committed so the repo
// is always buildable. Lines that match no block above render as plain
// paragraphs, so a future docs edit with a new construct degrades
// gracefully instead of failing the build.
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const mdPath = join(here, '..', '..', '..', 'docs', 'INTEGRATIONS.md')
const outPath = join(here, '..', 'src', 'docs-integrations.ts')

const escapeHtml = (s) =>
  s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')

// Inline markdown on one already-escaped line: code spans are protected
// first so their contents are never re-processed as bold or links.
function inline(line) {
  const codeSpans = []
  const withPlaceholders = line.replace(/`([^`]+)`/g, (_, code) => {
    codeSpans.push(`<code>${escapeHtml(code)}</code>`)
    return `\u0000${codeSpans.length - 1}\u0000`
  })
  let out = escapeHtml(withPlaceholders)
  out = out.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
  out = out.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, '<a href="$2">$1</a>')
  out = out.replace(/\u0000(\d+)\u0000/g, (_, i) => codeSpans[Number(i)])
  return out
}

const slug = (text) =>
  text.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '')

function tableRows(lines, i) {
  const cells = (row) =>
    row.trim().replace(/^\||\|$/g, '').split('|').map((c) => inline(c.trim()))
  const header = cells(lines[i])
  const body = []
  let j = i + 2 // skip the |---|---| separator
  while (j < lines.length && /^\s*\|/.test(lines[j])) {
    body.push(cells(lines[j]))
    j++
  }
  const head = `<thead><tr>${header.map((c) => `<th>${c}</th>`).join('')}</tr></thead>`
  const rows = body.map((r) => `<tr>${r.map((c) => `<td>${c}</td>`).join('')}</tr>`).join('')
  return [`<div class="table-scroll"><table>${head}<tbody>${rows}</tbody></table></div>`, j]
}

function convert(md) {
  const lines = md.replace(/\r\n/g, '\n').split('\n')
  const html = []
  let i = 0
  while (i < lines.length) {
    const line = lines[i]
    if (!line.trim()) { i++; continue }
    const fence = line.match(/^```(\w*)\s*$/)
    if (fence) {
      const buf = []
      i++
      while (i < lines.length && !lines[i].startsWith('```')) { buf.push(lines[i]); i++ }
      i++ // closing fence
      const lang = fence[1] ? ` class="language-${escapeHtml(fence[1])}"` : ''
      html.push(`<pre><code${lang}>${escapeHtml(buf.join('\n'))}</code></pre>`)
      continue
    }
    const heading = line.match(/^(#{1,3})\s+(.*)$/)
    if (heading) {
      const level = heading[1].length
      const text = inline(heading[2].trim())
      const plain = heading[2].trim().replace(/`([^`]+)`/g, '$1')
      html.push(`<h${level} id="${slug(plain)}">${text}</h${level}>`)
      i++
      continue
    }
    if (/^\s*\|/.test(line) && /^\s*\|[\s:|-]+\|/.test(lines[i + 1] ?? '')) {
      const [table, next] = tableRows(lines, i)
      html.push(table)
      i = next
      continue
    }
    if (/^---+$/.test(line.trim())) { html.push('<hr>'); i++; continue }
    const ordered = line.match(/^\d+\.\s+(.*)$/)
    const unordered = line.match(/^[-*]\s+(.*)$/)
    if (ordered || unordered) {
      const tag = ordered ? 'ol' : 'ul'
      const items = []
      while (i < lines.length) {
        const m = ordered
          ? lines[i].match(/^\d+\.\s+(.*)$/)
          : lines[i].match(/^[-*]\s+(.*)$/)
        if (!m) break
        items.push(`<li>${inline(m[1].trim())}</li>`)
        i++
      }
      html.push(`<${tag}>${items.join('')}</${tag}>`)
      continue
    }
    // Paragraph: consecutive non-blank lines that start no other block.
    // Blockquotes are not rendered as such; they fall through here as text.
    const buf = [line.trim()]
    i++
    while (i < lines.length && lines[i].trim() &&
      !lines[i].match(/^(#{1,3}\s|```|\d+\.\s+|[-*]\s+|---+$)/) &&
      !/^\s*\|/.test(lines[i])) {
      buf.push(lines[i].trim())
      i++
    }
    html.push(`<p>${inline(buf.join(' '))}</p>`)
  }
  return html.join('\n')
}

const md = readFileSync(mdPath, 'utf8')
const html = convert(md)
const module = `// Generated from docs/INTEGRATIONS.md by scripts/build-docs.mjs. Do not edit by hand.\nexport const INTEGRATION_DOCS_HTML: string = ${JSON.stringify(html)}\n`
const prev = (() => { try { return readFileSync(outPath, 'utf8') } catch { return '' } })()
if (prev !== module) {
  writeFileSync(outPath, module)
  console.log(`docs: wrote ${outPath} (${html.length} chars of HTML)`)
} else {
  console.log('docs: up to date')
}
