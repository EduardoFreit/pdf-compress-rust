// Web Worker: roda a compressão (Rust/WASM) fora da thread principal,
// para que arquivos grandes não travem a interface.
import init, { compress_image, compress_pdf } from '../pkg/projeto_02.js';

const ready = init();

self.onmessage = async ({ data: job }) => {
    const { id, kind, buffer, quality, scale, format, pdf } = job;
    try {
        await ready;
        const input = new Uint8Array(buffer);
        const start = performance.now();
        const r = kind === 'pdf'
            ? compress_pdf(input, quality, scale, pdf.stripStructure, pdf.stripMetadata, pdf.subsetFonts, pdf.maxCompression)
            : compress_image(input, quality, scale, format);
        const ms = performance.now() - start;

        const out = r.data; // Uint8Array copiado da memória do WASM
        const result = {
            mime: r.mime,
            originalSize: r.originalSize,
            compressedSize: r.compressedSize,
            width: r.width,
            height: r.height,
            originalWidth: r.originalWidth,
            originalHeight: r.originalHeight,
            imagesFound: r.imagesFound,
            imagesOptimized: r.imagesOptimized,
            fontsSubset: r.fontsSubset,
            objectsBefore: r.objectsBefore,
            objectsAfter: r.objectsAfter,
            keptOriginal: r.keptOriginal,
            ms,
            data: out.buffer,
        };
        r.free();
        self.postMessage({ id, ok: true, result }, [out.buffer]);
    } catch (err) {
        self.postMessage({ id, ok: false, error: String(err?.message ?? err) });
    }
};
