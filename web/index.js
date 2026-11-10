// Interface: envia os arquivos para o Web Worker (que roda o WASM) e mostra os resultados.

const $ = (id) => document.getElementById(id);
const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });

const els = {
    drop: $('drop'), input: $('fileInput'),
    quality: $('quality'), qualityOut: $('qualityOut'),
    scale: $('scale'), scaleOut: $('scaleOut'),
    format: $('format'), dryRun: $('dryRun'),
    subsetFonts: $('subsetFonts'), stripStructure: $('stripStructure'),
    stripMetadata: $('stripMetadata'), maxCompression: $('maxCompression'),
    rerun: $('rerun'), clear: $('clear'),
    summary: $('summary'), results: $('results'),
    dialog: $('preview'), previewTitle: $('previewTitle'),
    before: $('before'), after: $('after'),
    capBefore: $('capBefore'), capAfter: $('capAfter'),
};

/** @type {Map<number, Entry>} */
const entries = new Map();
const pending = new Map(); // id do job -> resolve
let nextId = 1;

// ---------- utilidades ----------

function formatBytes(n) {
    if (n < 1024) return `${n} B`;
    if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 ** 2).toFixed(2)} MB`;
}

function reduction(orig, comp) {
    return orig > 0 ? (1 - comp / orig) * 100 : 0;
}

function detectKind(file) {
    if (file.type === 'application/pdf' || /\.pdf$/i.test(file.name)) return 'pdf';
    if (file.type.startsWith('image/') || /\.(jpe?g|png|gif|bmp|webp|tiff?|ico|tga|qoi)$/i.test(file.name)) return 'image';
    return null;
}

function outputName(name, mime) {
    const base = name.replace(/\.[^.]+$/, '');
    const ext = { 'image/jpeg': 'jpg', 'image/png': 'png', 'application/pdf': 'pdf' }[mime]
        ?? name.split('.').pop();
    return `${base}-otimizado.${ext}`;
}

function settings() {
    return {
        quality: Number(els.quality.value),
        scale: Number(els.scale.value) / 100,
        format: els.format.value,
        pdf: {
            subsetFonts: els.subsetFonts.checked,
            stripStructure: els.stripStructure.checked,
            stripMetadata: els.stripMetadata.checked,
            maxCompression: els.maxCompression.checked,
        },
    };
}

// ---------- comunicação com o worker ----------

worker.onmessage = ({ data }) => {
    const resolve = pending.get(data.id);
    pending.delete(data.id);
    resolve?.(data);
};

worker.onerror = (e) => {
    console.error(e);
    for (const [id, resolve] of pending) resolve({ id, ok: false, error: 'Falha ao carregar o módulo WebAssembly.' });
    pending.clear();
};

async function runJob(entry) {
    const id = nextId++;
    const buffer = await entry.file.arrayBuffer();
    const job = { id, kind: entry.kind, buffer, ...settings() };
    return new Promise((resolve) => {
        pending.set(id, resolve);
        worker.postMessage(job, [buffer]);
    });
}

// ---------- fluxo principal ----------

async function process(entry) {
    entry.status = 'working';
    entry.error = null;
    render(entry);
    const msg = await runJob(entry);
    if (!entries.has(entry.id)) return; // removido enquanto processava

    if (entry.resultUrl) URL.revokeObjectURL(entry.resultUrl);
    if (msg.ok) {
        entry.result = msg.result;
        entry.resultUrl = URL.createObjectURL(new Blob([msg.result.data], { type: msg.result.mime }));
        entry.status = 'done';
    } else {
        entry.result = null;
        entry.resultUrl = null;
        entry.error = msg.error;
        entry.status = 'error';
    }
    render(entry);
    renderSummary();
}

function addFiles(files) {
    const added = [];
    for (const file of files) {
        const kind = detectKind(file);
        const entry = {
            id: nextId++, file, kind,
            originalUrl: URL.createObjectURL(file),
            status: kind ? 'queued' : 'error',
            error: kind ? null : 'Tipo de arquivo não suportado.',
            result: null, resultUrl: null, card: null,
        };
        entries.set(entry.id, entry);
        render(entry);
        if (kind) added.push(entry);
    }
    updateButtons();
    // O worker processa um arquivo por vez; a interface continua responsiva.
    Promise.all(added.map(process));
}

function rerunAll() {
    Promise.all([...entries.values()].filter((e) => e.kind).map(process));
}

function clearAll() {
    for (const e of entries.values()) {
        URL.revokeObjectURL(e.originalUrl);
        if (e.resultUrl) URL.revokeObjectURL(e.resultUrl);
    }
    entries.clear();
    els.results.replaceChildren();
    renderSummary();
    updateButtons();
}

// ---------- renderização ----------

function el(tag, attrs = {}, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
        if (k === 'class') node.className = v;
        else if (k.startsWith('on')) node.addEventListener(k.slice(2), v);
        else node.setAttribute(k, v);
    }
    node.append(...children.filter((c) => c != null));
    return node;
}

function render(entry) {
    const { file, result, status } = entry;
    const dry = els.dryRun.checked;

    const thumb = entry.kind === 'image'
        ? el('img', { class: 'thumb', src: entry.originalUrl, alt: '' })
        : el('div', { class: 'thumb' }, entry.kind === 'pdf' ? 'PDF' : '?');

    const info = el('div', { class: 'info' }, el('div', { class: 'name', title: file.name }, file.name));

    if (status === 'queued' || status === 'working') {
        info.append(el('div', { class: 'meta' }, el('span', { class: 'spinner' }), ` ${status === 'queued' ? 'Na fila' : 'Otimizando'}… (${formatBytes(file.size)})`));
    } else if (status === 'error') {
        info.append(el('div', { class: 'meta error' }, entry.error));
    } else {
        const pct = reduction(result.originalSize, result.compressedSize);
        const details = [];
        if (entry.kind === 'image') {
            details.push(`${result.originalWidth}×${result.originalHeight}`
                + (result.width !== result.originalWidth ? ` → ${result.width}×${result.height}` : ''));
            details.push(result.mime.replace('image/', '').toUpperCase());
        } else {
            details.push(`${result.imagesOptimized} de ${result.imagesFound} imagens recomprimidas`);
            if (result.fontsSubset) details.push(`${result.fontsSubset} ${result.fontsSubset === 1 ? 'fonte reduzida' : 'fontes reduzidas'}`);
            if (result.objectsAfter < result.objectsBefore) {
                details.push(`${result.objectsBefore.toLocaleString('pt-BR')} → ${result.objectsAfter.toLocaleString('pt-BR')} objetos`);
            }
        }
        details.push(`${result.ms.toFixed(0)} ms`);
        if (result.keptOriginal) details.push('já estava otimizado; original mantido');

        info.append(
            el('div', { class: 'sizes' },
                el('span', {}, `Original: ${formatBytes(result.originalSize)}`),
                el('span', {}, `Otimizado: ${formatBytes(result.compressedSize)}`),
                el('span', { class: pct > 0.05 ? 'gain' : 'gain none' }, pct > 0.05 ? `−${pct.toFixed(1)}%` : 'sem redução')),
            el('div', { class: 'bar', title: 'Tamanho otimizado em relação ao original' },
                el('i', { style: `width:${Math.max(1, 100 - pct)}%` })),
            el('div', { class: 'meta' }, details.join(' · ')),
        );
    }

    const actions = el('div', { class: 'actions' });
    if (status === 'done') {
        actions.append(el('button', { class: 'ghost', onclick: () => openPreview(entry) }, 'Comparar'));
        if (!dry) {
            actions.append(el('a', { class: 'btn', href: entry.resultUrl, download: outputName(file.name, result.mime) }, 'Baixar'));
        }
    }
    actions.append(el('button', { class: 'ghost', onclick: () => removeEntry(entry) }, 'Remover'));

    const card = el('article', { class: 'card' }, thumb, info, actions);
    if (entry.card) entry.card.replaceWith(card); else els.results.append(card);
    entry.card = card;
}

function removeEntry(entry) {
    URL.revokeObjectURL(entry.originalUrl);
    if (entry.resultUrl) URL.revokeObjectURL(entry.resultUrl);
    entry.card?.remove();
    entries.delete(entry.id);
    renderSummary();
    updateButtons();
}

function renderSummary() {
    const done = [...entries.values()].filter((e) => e.status === 'done');
    if (!done.length) { els.summary.hidden = true; return; }
    const orig = done.reduce((s, e) => s + e.result.originalSize, 0);
    const comp = done.reduce((s, e) => s + e.result.compressedSize, 0);
    const stat = (value, label) => el('div', {}, el('b', {}, value), el('span', {}, label));
    els.summary.replaceChildren(
        stat(String(done.length), done.length === 1 ? 'arquivo' : 'arquivos'),
        stat(formatBytes(orig), 'tamanho original'),
        stat(formatBytes(comp), 'tamanho otimizado'),
        stat(`${reduction(orig, comp).toFixed(1)}%`, `economia (${formatBytes(Math.max(0, orig - comp))})`),
    );
    if (els.dryRun.checked) els.summary.append(el('span', { class: 'badge' }, 'Dry-run: nenhum arquivo salvo'));
    els.summary.hidden = false;
}

function updateButtons() {
    const has = entries.size > 0;
    els.rerun.disabled = !has;
    els.clear.disabled = !has;
}

function openPreview(entry) {
    const { result } = entry;
    els.previewTitle.textContent = entry.file.name;
    els.capBefore.textContent = `Original — ${formatBytes(result.originalSize)}`;
    els.capAfter.textContent = `Otimizado — ${formatBytes(result.compressedSize)}`;
    const view = (url) => entry.kind === 'pdf'
        ? el('iframe', { src: url, title: 'Pré-visualização do PDF' })
        : el('img', { src: url, alt: '' });
    els.before.replaceChildren(view(entry.originalUrl));
    els.after.replaceChildren(view(entry.resultUrl));
    els.dialog.showModal();
}

// ---------- eventos ----------

els.input.addEventListener('change', () => {
    addFiles(els.input.files);
    els.input.value = '';
});

for (const type of ['dragenter', 'dragover']) {
    els.drop.addEventListener(type, (e) => { e.preventDefault(); els.drop.classList.add('over'); });
}
for (const type of ['dragleave', 'drop']) {
    els.drop.addEventListener(type, () => els.drop.classList.remove('over'));
}
els.drop.addEventListener('drop', (e) => {
    e.preventDefault();
    addFiles(e.dataTransfer.files);
});

els.quality.addEventListener('input', () => { els.qualityOut.textContent = els.quality.value; });
els.scale.addEventListener('input', () => { els.scaleOut.textContent = `${els.scale.value}%`; });
els.dryRun.addEventListener('change', () => {
    for (const e of entries.values()) render(e);
    renderSummary();
});
els.rerun.addEventListener('click', rerunAll);
els.clear.addEventListener('click', clearAll);
$('closePreview').addEventListener('click', () => els.dialog.close());
els.dialog.addEventListener('close', () => { els.before.replaceChildren(); els.after.replaceChildren(); });
