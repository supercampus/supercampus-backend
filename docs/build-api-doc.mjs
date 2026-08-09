// Renders the OpenAPI contract into a print-ready HTML document.
import fs from 'node:fs';

const SPEC = process.argv[2];
const OUT = process.argv[3];
const spec = JSON.parse(fs.readFileSync(SPEC, 'utf8'));

const esc = (v) => String(v ?? '').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
const slug = (v) => v.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
const refName = (ref) => (typeof ref === 'string' ? ref.split('/').pop() : null);

const METHODS = ['get', 'post', 'put', 'patch', 'delete', 'options', 'head'];

// Group operations by their first tag, preserving the spec's declared tag order.
const declaredTags = (spec.tags ?? []).map((t) => t.name);
const groups = new Map(declaredTags.map((name) => [name, []]));
let operationCount = 0;
for (const [path, item] of Object.entries(spec.paths)) {
  for (const method of METHODS) {
    const op = item[method];
    if (!op || typeof op !== 'object') continue;
    operationCount += 1;
    const tag = (op.tags ?? ['Other'])[0];
    if (!groups.has(tag)) groups.set(tag, []);
    groups.get(tag).push({ method, path, op, params: item.parameters ?? [] });
  }
}
for (const [, ops] of groups) ops.sort((a, b) => a.path.localeCompare(b.path) || a.method.localeCompare(b.method));

const tagDescription = Object.fromEntries((spec.tags ?? []).map((t) => [t.name, t.description ?? '']));

function schemaRefOf(mediaObject) {
  const schema = mediaObject?.content?.['application/json']?.schema;
  if (!schema) return null;
  if (schema.$ref) return refName(schema.$ref);
  if (schema.items?.$ref) return `${refName(schema.items.$ref)}[]`;
  return schema.type ?? null;
}

function securityLabel(op) {
  const security = op.security ?? spec.security ?? [];
  if (Array.isArray(security) && security.length === 0) return 'Public';
  const names = security.flatMap((entry) => Object.keys(entry));
  return names.length ? names.join(' or ') : 'Public';
}

function renderParams(all) {
  if (!all.length) return '';
  const rows = all.map((p) => `<tr>
      <td><code>${esc(p.name)}</code></td>
      <td>${esc(p.in)}</td>
      <td>${p.required ? 'yes' : 'no'}</td>
      <td>${esc(p.schema?.type ?? refName(p.schema?.$ref) ?? '')}</td>
      <td>${esc(p.description ?? '')}</td>
    </tr>`).join('');
  return `<table class="tbl"><thead><tr><th>Name</th><th>In</th><th>Required</th><th>Type</th><th>Description</th></tr></thead><tbody>${rows}</tbody></table>`;
}

function renderResponses(op) {
  const entries = Object.entries(op.responses ?? {});
  if (!entries.length) return '';
  const rows = entries.map(([code, res]) => {
    const resolved = res.$ref ? spec.components?.responses?.[refName(res.$ref)] ?? {} : res;
    const body = schemaRefOf(resolved);
    return `<tr>
      <td><span class="status s${String(code)[0]}">${esc(code)}</span></td>
      <td>${esc(resolved.description ?? refName(res.$ref) ?? '')}</td>
      <td>${body ? `<code>${esc(body)}</code>` : '&mdash;'}</td>
    </tr>`;
  }).join('');
  return `<table class="tbl"><thead><tr><th>Code</th><th>Meaning</th><th>Body</th></tr></thead><tbody>${rows}</tbody></table>`;
}

function renderOperation({ method, path, op, params }) {
  const all = [...params, ...(op.parameters ?? [])];
  const requestBody = schemaRefOf(op.requestBody);
  const required = op.requestBody?.required;
  return `<section class="op">
    <div class="op-head">
      <span class="verb v-${method}">${method.toUpperCase()}</span>
      <code class="op-path">${esc(path)}</code>
    </div>
    ${op.summary ? `<p class="op-summary">${esc(op.summary)}</p>` : ''}
    <p class="meta"><strong>Auth:</strong> ${esc(securityLabel(op))}${op.operationId ? ` &nbsp;·&nbsp; <strong>Operation:</strong> <code>${esc(op.operationId)}</code>` : ''}</p>
    ${op.description && op.description !== op.summary ? `<p class="op-desc">${esc(op.description)}</p>` : ''}
    ${all.length ? `<h5>Parameters</h5>${renderParams(all)}` : ''}
    ${requestBody ? `<h5>Request body${required ? ' <span class="req">required</span>' : ''}</h5><p class="body-ref"><code>${esc(requestBody)}</code></p>` : ''}
    <h5>Responses</h5>${renderResponses(op)}
  </section>`;
}

function renderSchemaProps(name, schema) {
  const props = schema.properties ?? {};
  const requiredSet = new Set(schema.required ?? []);
  const keys = Object.keys(props);
  const rows = keys.length
    ? keys.map((key) => {
        const p = props[key];
        let type = p.$ref ? refName(p.$ref) : Array.isArray(p.type) ? p.type.join(' | ') : (p.type ?? '');
        if (p.type === 'array') type = `${p.items?.$ref ? refName(p.items.$ref) : p.items?.type ?? 'any'}[]`;
        const notes = [
          p.format ? `format: ${p.format}` : '',
          p.minLength !== undefined ? `minLength: ${p.minLength}` : '',
          p.writeOnly ? 'write-only' : '',
          p.description ?? '',
        ].filter(Boolean).join('; ');
        return `<tr><td><code>${esc(key)}</code></td><td>${esc(type)}</td><td>${requiredSet.has(key) ? 'yes' : 'no'}</td><td>${esc(notes)}</td></tr>`;
      }).join('')
    : '<tr><td colspan="4"><em>Free-form object</em></td></tr>';
  return `<section class="schema" id="schema-${slug(name)}">
    <h4>${esc(name)}</h4>
    ${schema.description ? `<p class="op-desc">${esc(schema.description)}</p>` : ''}
    <table class="tbl"><thead><tr><th>Property</th><th>Type</th><th>Required</th><th>Notes</th></tr></thead><tbody>${rows}</tbody></table>
  </section>`;
}

const schemas = spec.components?.schemas ?? {};
const securitySchemes = spec.components?.securitySchemes ?? {};

const toc = [...groups.entries()].filter(([, ops]) => ops.length).map(([tag, ops]) =>
  `<li><a href="#tag-${slug(tag)}">${esc(tag)}</a><span class="dots"></span><span class="count">${ops.length}</span></li>`).join('');

const sections = [...groups.entries()].filter(([, ops]) => ops.length).map(([tag, ops]) => `
  <section class="tag" id="tag-${slug(tag)}">
    <h2>${esc(tag)}</h2>
    ${tagDescription[tag] ? `<p class="tag-desc">${esc(tagDescription[tag])}</p>` : ''}
    ${ops.map(renderOperation).join('')}
  </section>`).join('');

const securityRows = Object.entries(securitySchemes).map(([name, s]) =>
  `<tr><td><code>${esc(name)}</code></td><td>${esc(s.type)}${s.scheme ? ` / ${esc(s.scheme)}` : ''}</td><td>${esc(s.in ? `${s.in}: ${s.name}` : s.bearerFormat ?? '')}</td><td>${esc(s.description ?? '')}</td></tr>`).join('');

const generated = process.env.DOC_DATE || '';

const html = `<!doctype html>
<html><head><meta charset="utf-8"><title>SuperCampus API Reference</title>
<style>
  @page { size: A4; margin: 16mm 14mm; }
  * { box-sizing: border-box; }
  body { font-family: "Segoe UI", Roboto, Helvetica, Arial, sans-serif; font-size: 9.5pt; line-height: 1.5; color: #17202a; margin: 0; }
  code { font-family: "Cascadia Mono", Consolas, monospace; font-size: 8.8pt; background: #f2f5f8; padding: 1px 4px; border-radius: 3px; }
  h1 { font-size: 26pt; margin: 0 0 6px; letter-spacing: -0.5px; }
  h2 { font-size: 15pt; margin: 0 0 4px; padding-bottom: 6px; border-bottom: 2px solid #0f766e; color: #0b3d2e; }
  h4 { font-size: 11pt; margin: 0 0 6px; color: #0b3d2e; }
  h5 { font-size: 8.5pt; text-transform: uppercase; letter-spacing: .07em; color: #667085; margin: 10px 0 4px; }
  .cover { height: 247mm; display: flex; flex-direction: column; justify-content: center; page-break-after: always; }
  .cover .rule { width: 70px; height: 5px; background: linear-gradient(90deg,#0f766e,#b9f43b); margin: 14px 0 18px; }
  .cover .sub { font-size: 12pt; color: #475467; margin: 0 0 26px; max-width: 135mm; }
  .facts { display: grid; grid-template-columns: repeat(2, 1fr); gap: 10px; max-width: 135mm; }
  .fact { border: 1px solid #dfe4ea; border-radius: 7px; padding: 10px 12px; }
  .fact small { display: block; text-transform: uppercase; font-size: 7.5pt; letter-spacing: .09em; color: #667085; }
  .fact strong { font-size: 12pt; color: #0b3d2e; }
  .note { margin-top: 22px; padding: 11px 13px; border-left: 3px solid #b9f43b; background: #f7fbef; font-size: 9pt; max-width: 135mm; }
  .toc { page-break-after: always; }
  .toc ul { list-style: none; padding: 0; margin: 0; column-count: 2; column-gap: 22px; }
  .toc li { display: flex; align-items: baseline; gap: 5px; margin-bottom: 5px; break-inside: avoid; }
  .toc a { color: #0b3d2e; text-decoration: none; font-weight: 600; }
  .dots { flex: 1; border-bottom: 1px dotted #c3ccd6; }
  .count { color: #667085; font-size: 8.5pt; }
  .tag { page-break-before: always; }
  .op { border: 1px solid #e3e8ee; border-radius: 8px; padding: 11px 13px; margin: 11px 0; page-break-inside: avoid; }
  .op-head { display: flex; align-items: center; gap: 9px; flex-wrap: wrap; }
  .verb { font-size: 7.8pt; font-weight: 800; color: #fff; padding: 2.5px 8px; border-radius: 4px; letter-spacing: .05em; }
  .v-get { background: #0f766e; } .v-post { background: #1d4ed8; } .v-put { background: #b45309; }
  .v-patch { background: #7c3aed; } .v-delete { background: #b91c1c; }
  .op-path { font-size: 9.6pt; font-weight: 600; background: none; padding: 0; }
  .op-summary { margin: 7px 0 3px; font-weight: 600; }
  .op-desc { margin: 4px 0; color: #475467; }
  .meta { margin: 3px 0; color: #667085; font-size: 8.6pt; }
  .req { color: #b91c1c; font-weight: 700; text-transform: none; }
  .body-ref { margin: 3px 0; }
  .tbl { width: 100%; border-collapse: collapse; margin: 4px 0 2px; font-size: 8.6pt; }
  .tbl th { text-align: left; background: #f6f8fa; color: #344054; font-weight: 700; padding: 4px 7px; border: 1px solid #e3e8ee; }
  .tbl td { padding: 4px 7px; border: 1px solid #e9edf2; vertical-align: top; }
  .status { font-weight: 800; font-size: 8.4pt; }
  .s2 { color: #0f766e; } .s4 { color: #b45309; } .s5 { color: #b91c1c; }
  .schema { page-break-inside: avoid; margin-bottom: 13px; }
  .tag-desc { color: #475467; margin: 6px 0 10px; }
</style></head><body>

<div class="cover">
  <h1>SuperCampus</h1>
  <div class="rule"></div>
  <div style="font-size:16pt;font-weight:600;color:#0b3d2e;margin-bottom:6px;">Backend API Reference</div>
  <p class="sub">${esc(spec.info.description ?? '')}</p>
  <div class="facts">
    <div class="fact"><small>Specification</small><strong>OpenAPI ${esc(spec.openapi)}</strong></div>
    <div class="fact"><small>API version</small><strong>${esc(spec.info.version)}</strong></div>
    <div class="fact"><small>Operations</small><strong>${operationCount}</strong></div>
    <div class="fact"><small>Schemas</small><strong>${Object.keys(schemas).length}</strong></div>
  </div>
  <div class="note"><strong>Base URL</strong> &nbsp;<code>${esc(spec.servers?.[0]?.url ?? '')}</code> &nbsp;(${esc(spec.servers?.[0]?.description ?? '')})<br>
  In deployment the browser calls the Next.js origin at <code>/api/*</code>, which proxies to this service. WebSocket upgrades are not proxied and must address this API directly.</div>
  ${generated ? `<p style="margin-top:20px;color:#667085;font-size:8.5pt;">Generated ${esc(generated)} from <code>docs/openapi.yaml</code></p>` : ''}
</div>

<div class="toc">
  <h2>Contents</h2>
  <ul>${toc}
    <li><a href="#authentication">Authentication schemes</a><span class="dots"></span><span class="count">${Object.keys(securitySchemes).length}</span></li>
    <li><a href="#schemas">Schema reference</a><span class="dots"></span><span class="count">${Object.keys(schemas).length}</span></li>
  </ul>
  <section id="authentication" style="margin-top:22px;">
    <h4>Authentication schemes</h4>
    <table class="tbl"><thead><tr><th>Scheme</th><th>Type</th><th>Carried in</th><th>Notes</th></tr></thead><tbody>${securityRows}</tbody></table>
  </section>
</div>

${sections}

<section class="tag" id="schemas">
  <h2>Schema reference</h2>
  ${Object.entries(schemas).map(([name, schema]) => renderSchemaProps(name, schema)).join('')}
</section>

</body></html>`;

fs.writeFileSync(OUT, html, 'utf8');
console.log(`wrote ${OUT} (${operationCount} operations, ${Object.keys(schemas).length} schemas, ${[...groups.values()].filter((o) => o.length).length} sections)`);
