// Interface: mantém uma fila de arquivos, envia um por vez ao Web Worker
// (que roda o WASM) e mostra os resultados.

const $ = (id) => document.getElementById(id);

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

/**
 * @typedef {{ id: number, file: File, kind: 'pdf'|'image'|null, originalUrl: string,
 *   status: 'queued'|'working'|'done'|'error'|'cancelled', error: string|null,
 *   result: object|null, resultUrl: string|null, card: HTMLElement|null, startedAt: number }} Entry
 * @typedef {{ id: number, entry: Entry, settings: object }} Job
 */

/** @type {Map<number, Entry>} */
const entries = new Map();
/** @type {Job[]} */
let queue = [];
/** @type {Job|null} */
let current = null;
let worker = null;
let nextEntryId = 1;
let nextJobId = 1;
let previewing = null; // entrada exibida no diálogo de comparação

// ---------- utilidades ----------

function formatBytes(n) {
    if (n < 1024) return `${n} B`;
    if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 ** 2).toFixed(2)} MB`;
}

function formatDuration(ms) {
    return ms < 1000 ? `${ms.toFixed(0)} ms` : `${(ms / 1000).toFixed(1)} s`;
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

// ---------- worker e fila ----------

function startWorker() {
    worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
    worker.onmessage = onWorkerMessage;
    worker.onerror = onWorkerError;
}

/** Encerra o worker (interrompendo o que estiver rodando) e cria outro. */
function restartWorker() {
    worker?.terminate();
    startWorker();
}

function onWorkerMessage({ data }) {
    // Mensagens de um job já cancelado são ignoradas.
    if (!current || data.id !== current.id) return;
    const job = current;
    current = null;
    if (data.fatal) restartWorker();
    finish(job, data);
    pump();
}

function onWorkerError(e) {
    // Erro fora do try/catch do worker (ex.: falha ao carregar o módulo).
    e.preventDefault?.();
    console.error(e);
    const job = current;
    current = null;
    restartWorker();
    if (job) finish(job, { ok: false, error: 'O processamento falhou: não foi possível executar o módulo WebAssembly.' });
    pump();
}

/** Inicia o próximo job da fila, se o worker estiver livre. */
async function pump() {
    if (current || queue.length === 0) return;
    const job = queue.shift();
    current = job;
    const { entry } = job;
    entry.status = 'working';
    entry.startedAt = performance.now();
    render(entry);

    let buffer;
    try {
        buffer = await entry.file.arrayBuffer();
    } catch {
        if (current !== job) return; // cancelado enquanto lia
        current = null;
        finish(job, { ok: false, error: 'Não foi possível ler o arquivo (ele foi movido ou apagado?).' });
        pump();
        return;
    }
    if (current !== job) return; // cancelado enquanto lia
    worker.postMessage({ id: job.id, kind: entry.kind, buffer, ...job.settings }, [buffer]);
}

/** Coloca a entrada na fila com as configurações atuais (cancelando execuções anteriores dela). */
function enqueue(entry) {
    cancel(entry, false);
    entry.status = 'queued';
    entry.error = null;
    queue.push({ id: nextJobId++, entry, settings: settings() });
    render(entry);
}

/**
 * Tira a entrada da fila e, se ela estiver em execução, interrompe o worker.
 * Retorna true se havia algo a cancelar.
 */
function cancel(entry, startNext = true) {
    const before = queue.length;
    queue = queue.filter((job) => job.entry !== entry);
    let cancelled = queue.length !== before;
    if (current?.entry === entry) {
        current = null;
        restartWorker();
        cancelled = true;
    }
    if (startNext) pump();
    return cancelled;
}

function finish(job, msg) {
    const { entry } = job;
    if (!entries.has(entry.id)) return; // removido enquanto processava

    if (entry.resultUrl) URL.revokeObjectURL(entry.resultUrl);
    entry.resultUrl = null;
    entry.result = null;
    if (msg.ok) {
        entry.result = msg.result;
        entry.resultUrl = URL.createObjectURL(new Blob([msg.result.data], { type: msg.result.mime }));
        entry.status = 'done';
    } else {
        entry.error = msg.error;
        entry.status = 'error';
    }
    if (previewing === entry) els.dialog.close();
    render(entry);
    renderSummary();
    updateButtons();
}

// ---------- ações ----------

function addFiles(files) {
    for (const file of files) {
        const kind = detectKind(file);
        const entry = {
            id: nextEntryId++, file, kind,
            originalUrl: URL.createObjectURL(file),
            status: 'error',
            error: kind ? null : 'Tipo de arquivo não suportado.',
            result: null, resultUrl: null, card: null, startedAt: 0,
        };
        entries.set(entry.id, entry);
        if (kind) enqueue(entry); else render(entry);
    }
    updateButtons();
    pump();
}

function rerunAll() {
    for (const entry of entries.values()) {
        if (entry.kind) enqueue(entry);
    }
    renderSummary();
    updateButtons();
    pump();
}

function cancelEntry(entry) {
    cancel(entry);
    entry.status = 'cancelled';
    render(entry);
    updateButtons();
}

function disposeEntry(entry) {
    if (previewing === entry) els.dialog.close();
    URL.revokeObjectURL(entry.originalUrl);
    if (entry.resultUrl) URL.revokeObjectURL(entry.resultUrl);
    entry.card?.remove();
    entries.delete(entry.id);
}

function removeEntry(entry) {
    cancel(entry);
    disposeEntry(entry);
    renderSummary();
    updateButtons();
}

function clearAll() {
    queue = [];
    if (current) {
        current = null;
        restartWorker();
    }
    for (const entry of [...entries.values()]) disposeEntry(entry);
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

    if (status === 'queued') {
        info.append(el('div', { class: 'meta' }, `Na fila (${formatBytes(file.size)})`));
    } else if (status === 'working') {
        info.append(el('div', { class: 'meta' },
            el('span', { class: 'spinner' }),
            ` Otimizando… (${formatBytes(file.size)}) `,
            el('span', { class: 'elapsed', 'data-start': String(entry.startedAt) }, formatDuration(performance.now() - entry.startedAt))));
    } else if (status === 'cancelled') {
        info.append(el('div', { class: 'meta' }, 'Cancelado.'));
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
        details.push(formatDuration(result.ms));
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
        actions.append(el('button', { type: 'button', class: 'ghost', onclick: () => openPreview(entry) }, 'Comparar'));
        if (!dry) {
            actions.append(el('a', { class: 'btn', href: entry.resultUrl, download: outputName(file.name, result.mime) }, 'Baixar'));
        }
    }
    if (status === 'queued' || status === 'working') {
        actions.append(el('button', { type: 'button', class: 'ghost', onclick: () => cancelEntry(entry) }, 'Cancelar'));
    }
    if (entry.kind && (status === 'cancelled' || status === 'error')) {
        actions.append(el('button', { type: 'button', class: 'ghost', onclick: () => { enqueue(entry); updateButtons(); pump(); } }, 'Tentar de novo'));
    }
    actions.append(el('button', { type: 'button', class: 'ghost', onclick: () => removeEntry(entry) }, 'Remover'));

    const card = el('article', { class: 'card' }, thumb, info, actions);
    if (entry.card?.isConnected) entry.card.replaceWith(card); else els.results.append(card);
    entry.card = card;
}

function renderSummary() {
    const done = [...entries.values()].filter((e) => e.status === 'done');
    if (!done.length) {
        els.summary.hidden = true;
        els.summary.replaceChildren();
        return;
    }
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
    els.rerun.disabled = ![...entries.values()].some((e) => e.kind);
    els.clear.disabled = entries.size === 0;
}

function openPreview(entry) {
    const { result } = entry;
    previewing = entry;
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

// Atualiza o cronômetro do arquivo em processamento.
setInterval(() => {
    for (const span of document.querySelectorAll('.elapsed[data-start]')) {
        span.textContent = formatDuration(performance.now() - Number(span.dataset.start));
    }
}, 500);

// ---------- eventos ----------

els.input.addEventListener('change', () => {
    addFiles(els.input.files);
    els.input.value = '';
});

// Área de upload acessível pelo teclado.
els.drop.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        els.input.click();
    }
});

// Soltar arquivos em qualquer lugar da página (e nunca deixar o navegador abri-los,
// o que faria perder o trabalho em andamento).
let dragDepth = 0;
window.addEventListener('dragenter', (e) => {
    e.preventDefault();
    if (++dragDepth === 1) els.drop.classList.add('over');
});
window.addEventListener('dragleave', () => {
    if (--dragDepth <= 0) { dragDepth = 0; els.drop.classList.remove('over'); }
});
window.addEventListener('dragover', (e) => e.preventDefault());
window.addEventListener('drop', (e) => {
    e.preventDefault();
    dragDepth = 0;
    els.drop.classList.remove('over');
    if (e.dataTransfer?.files.length) addFiles(e.dataTransfer.files);
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
els.dialog.addEventListener('close', () => {
    previewing = null;
    els.before.replaceChildren();
    els.after.replaceChildren();
});

startWorker();
updateButtons();
