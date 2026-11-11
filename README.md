# Otimizador de Arquivos — Rust + WebAssembly

Aplicação web que **reduz o tamanho de imagens e PDFs direto no navegador**. Toda a
lógica de compressão é escrita em Rust e compilada para WebAssembly: os arquivos
**nunca saem do computador do usuário**, não há servidor processando nada.

**Demonstração:** https://USUARIO.github.io/REPOSITORIO/

| Exemplo | Original | Otimizado | Redução |
|---|---:|---:|---:|
| Planilha do Excel exportada em PDF (132 páginas) | 3,9 MB | 546 KB | **−86%** |
| A mesma planilha, com "compressão máxima" | 3,9 MB | 497 KB | **−87%** |
| Foto PNG 2400×1600 (qualidade 70, resolução 60%) | 4,15 MB | 160 KB | **−96%** |

No caso do PDF, o resultado é **idêntico pixel a pixel** ao original: nada visível é perdido.

---

## Sumário

- [Funcionalidades](#funcionalidades)
- [Como usar](#como-usar)
- [Como funciona](#como-funciona)
  - [Imagens](#imagens)
  - [PDFs](#pdfs)
  - [Interface e Web Worker](#interface-e-web-worker)
- [Rodando localmente](#rodando-localmente)
- [Linha de comando](#linha-de-comando)
- [Testes](#testes)
- [Publicação no GitHub Pages](#publicação-no-github-pages)
- [Estrutura do projeto](#estrutura-do-projeto)
- [Limitações conhecidas](#limitações-conhecidas)
- [Tecnologias](#tecnologias)

---

## Funcionalidades

- **Upload de vários arquivos** por clique ou arrastando para qualquer parte da página.
- **Compressão de imagens** (JPEG, PNG, GIF, BMP, WebP, TIFF...): qualidade JPEG,
  resolução e formato de saída configuráveis. Imagens com transparência continuam em PNG.
- **Otimização de PDFs** muito além das imagens:
  - recompressão das imagens embutidas;
  - redução das fontes aos caracteres realmente usados (*font subsetting*);
  - remoção das tags de acessibilidade e de metadados (opcional);
  - regravação compacta com *object streams* e tabela xref comprimida.
- **Resumo em tempo real**: tamanho original × otimizado, percentual de redução, tempo e detalhes de cada arquivo.
- **Pré-visualização lado a lado** do original e do resultado (imagens e PDFs).
- **Modo dry-run**: compara original e otimizado sem oferecer download, para testar configurações.
- **Fila com cancelamento**: um arquivo por vez, com botões Cancelar, Tentar de novo e Remover, sem travar a interface.
- **Segurança do resultado**: se a versão otimizada não ficar menor, o original é mantido; se o PDF gerado não puder ser relido com as mesmas páginas, o processo é abortado.

## Como usar

1. Abra a [demonstração](https://USUARIO.github.io/REPOSITORIO/) (ou [rode localmente](#rodando-localmente)).
2. Ajuste as opções, se quiser:

   | Opção | Efeito |
   |---|---|
   | **Qualidade JPEG** | 10–100. Valores menores geram arquivos menores, com mais perda visual. |
   | **Resolução** | Reduz largura e altura das imagens (também das imagens dentro de PDFs). |
   | **Formato das imagens** | Automático (PNG se houver transparência, senão JPEG), JPEG ou PNG. |
   | **Modo dry-run** | Mostra a comparação sem oferecer o download. |
   | **Reduzir fontes** | Mantém só os glifos usados nas fontes TrueType embutidas no PDF. |
   | **Remover tags de acessibilidade** | Apaga a árvore de estrutura do PDF (leitores de tela perdem a estrutura do documento). |
   | **Remover metadados** | Apaga metadados XMP, dados privados de aplicativos e miniaturas de página. |
   | **Compressão máxima** | Usa o algoritmo Zopfli: ~10% menor, porém dezenas de vezes mais lento. |

3. Arraste os arquivos. Cada um mostra o progresso e, ao terminar, os botões **Comparar** e **Baixar**.
4. Mudou as opções? Clique em **Reprocessar com estas configurações**.

## Como funciona

### Imagens

`src/image_opt.rs`

1. Detecta o formato pelo conteúdo (não pela extensão) e decodifica com a crate [`image`](https://crates.io/crates/image).
2. Redimensiona pelo fator de escala escolhido (filtro triangular, bom equilíbrio entre qualidade e velocidade).
3. Escolhe o formato de saída: no modo automático, **PNG** quando há pixels transparentes e **JPEG** caso contrário.
   Ao converter para JPEG, a transparência é composta sobre fundo branco.
4. Codifica: JPEG com a qualidade escolhida; PNG com compressão máxima e filtro adaptativo.
   Imagens em tons de cinza são gravadas com 1 canal.
5. Se o resultado não ficar menor (e nada foi redimensionado nem convertido), devolve o original.

### PDFs

`src/pdf/`

Um PDF é um conjunto de objetos (páginas, fontes, imagens, streams de desenho...).
Documentos exportados pelo Office costumam ter pouco peso em imagens e muito peso em
estrutura, fontes e na forma como o arquivo é gravado. Por isso o otimizador ataca várias frentes:

| Etapa | Arquivo | O que faz |
|---|---|---|
| 1. Imagens | `images.rs` | Extrai imagens JPEG (DCT), RGB/cinza cruas (Flate/LZW), ICC e com paleta; recomprime como JPEG com a mesma rotina das imagens comuns. Pula com segurança o que não sabe tratar (CMYK, JBIG2, 1 bit, máscaras). Só substitui quando fica menor. |
| 2. Estrutura | `content.rs` | Remove a árvore de tags (`StructTreeRoot`) e as marcações `BDC/EMC` nos streams de página, **preservando** o conteúdo opcional (`/OC`, camadas). |
| 3. Metadados | `content.rs` | Remove XMP, `PieceInfo` e miniaturas de página. |
| 4. Fontes | `fonts.rs`, `truetype.rs` | Percorre páginas, Form XObjects, aparências de anotações e padrões para descobrir quais glifos de cada fonte TrueType são usados; depois esvazia os glifos não usados e descarta tabelas que leitores de PDF não consultam. Fontes que não podem ser analisadas com segurança (ex.: de formulários) ficam intactas. |
| 5. Limpeza | `mod.rs` | Junta streams idênticos, remove objetos órfãos e renumera. |
| 6. Recompressão | `mod.rs` | Recomprime todos os streams com zlib no nível máximo (ou Zopfli). |
| 7. Gravação | `writer.rs` | Grava no formato PDF 1.5: objetos pequenos agrupados em *object streams* comprimidos e tabela xref binária comprimida. |
| 8. Verificação | `mod.rs` | Relê o PDF gerado e confere o número de páginas antes de entregar. |

**Exemplo real** (planilha do Excel com 132 páginas):

| Componente | Antes | Depois |
|---|---:|---:|
| Tags de acessibilidade | 77.244 objetos (~2,5 MB) | removidas |
| Fontes Calibri embutidas | 445 KB | 42 KB |
| Streams de desenho das páginas | 656 KB | ~465 KB |
| **Arquivo final** | **3,9 MB** | **546 KB** |

> Por que um gravador próprio? A crate `lopdf` 0.35 grava cada objeto separadamente.
> Em PDFs com dezenas de milhares de objetos pequenos isso deixa o arquivo regravado
> *maior* que o original. O `writer.rs` resolve isso com *object streams*.

### Interface e Web Worker

`web/`

- `index.html` / `style.css`: interface responsiva, com tema claro e escuro.
- `index.js`: mantém a fila de arquivos, envia **um arquivo por vez** ao worker e atualiza os cartões e o resumo.
- `worker.js`: carrega o módulo WebAssembly e executa a compressão fora da thread principal, então a página continua responsiva.

Uma chamada ao WebAssembly é síncrona e não pode ser interrompida no meio. Para
**cancelar**, a página encerra o worker (`worker.terminate()`) e cria outro na hora.
O mesmo acontece depois de um erro interno, para que um arquivo problemático não trave a fila.

A API exposta pelo Rust (via [`wasm-bindgen`](https://github.com/rustwasm/wasm-bindgen)):

```ts
compress_image(data: Uint8Array, quality: number, scale: number, format: "auto" | "jpeg" | "png"): CompressionResult
compress_pdf(data: Uint8Array, quality: number, scale: number,
             stripStructure: boolean, stripMetadata: boolean,
             subsetFonts: boolean, maxCompression: boolean): CompressionResult

class CompressionResult {
  data: Uint8Array; mime: string;
  originalSize: number; compressedSize: number;
  width: number; height: number; originalWidth: number; originalHeight: number;
  imagesFound: number; imagesOptimized: number; fontsSubset: number;
  objectsBefore: number; objectsAfter: number; keptOriginal: boolean;
}
```

## Rodando localmente

### Pré-requisitos

- [Rust](https://rustup.rs/) 1.85 ou mais recente (o projeto usa a *edition* 2024)
- O alvo WebAssembly: `rustup target add wasm32-unknown-unknown`
- [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/): `cargo install wasm-pack`
- Qualquer servidor HTTP estático (os exemplos usam Python)

### Passos

```bash
git clone https://github.com/USUARIO/REPOSITORIO.git
cd REPOSITORIO

# 1. Compila o Rust para WebAssembly (gera a pasta pkg/)
wasm-pack build --target web --release

# 2. Serve a RAIZ do projeto (a página importa ../pkg/)
python -m http.server 8765
```

Abra **http://localhost:8765/web/**.

> Abrir o `index.html` direto do disco (`file://`) não funciona: o navegador bloqueia
> o Web Worker e o carregamento do `.wasm`. É preciso um servidor HTTP.

Sempre que alterar o código Rust, rode o `wasm-pack build` de novo e recarregue a página.

## Linha de comando

Os mesmos algoritmos podem ser usados sem o navegador. Nesse modo as imagens e os
streams são processados **em paralelo** (com `rayon`):

```bash
# Otimiza um PDF ou imagem: entrada, saída, [qualidade], [escala], [max]
cargo run --release --example compress -- entrada.pdf saida.pdf
cargo run --release --example compress -- entrada.pdf saida.pdf 60 0.5 max
cargo run --release --example compress -- foto.png foto-otimizada.jpg 75 0.5

# Mostra quanto espaço cada tipo de objeto ocupa num PDF (útil para investigar)
cargo run --release --example inspect -- arquivo.pdf
```

## Testes

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

Os testes ficam em `tests/optimizer.rs` e não dependem de nenhum arquivo externo:
as imagens e os PDFs de teste são gerados pelo próprio código. Eles cobrem:

- conversão PNG → JPEG, redimensionamento, efeito da qualidade e preservação de transparência;
- erros para entradas inválidas (em vez de *panic*);
- PDF com imagens JPEG, RGB crua e com paleta: todas recomprimidas e com dimensões corretas;
- PDF marcado (como os do Office): remoção da estrutura, das marcações e dos metadados, preservando o conteúdo opcional;
- opções desligadas preservam a estrutura.

## Publicação no GitHub Pages

O workflow `.github/workflows/pages.yml` roda a cada push na branch `main`:

1. **Testes**: `cargo clippy` e `cargo test`.
2. **Build**: instala o `wasm-pack`, compila o WebAssembly e monta o site (`web/` + `pkg/`).
3. **Publicação**: envia o site para o GitHub Pages.

Para ativar no seu fork, vá em **Settings → Pages → Build and deployment → Source** e escolha **GitHub Actions**.

## Estrutura do projeto

```
.
├── src/
│   ├── lib.rs            # API exposta ao JavaScript (wasm-bindgen)
│   ├── image_opt.rs      # compressão de imagens
│   └── pdf/
│       ├── mod.rs        # orquestra as etapas de otimização do PDF
│       ├── images.rs     # imagens embutidas no PDF
│       ├── content.rs    # tags de estrutura, marcações e metadados
│       ├── fonts.rs      # descobre os glifos usados em cada fonte
│       ├── truetype.rs   # gera o subconjunto de fontes TrueType
│       └── writer.rs     # gravação compacta (object streams + xref stream)
├── web/
│   ├── index.html        # interface
│   ├── index.js          # fila, renderização e eventos
│   ├── worker.js         # executa o WASM fora da thread principal
│   └── style.css
├── examples/
│   ├── compress.rs       # CLI de compressão
│   └── inspect.rs        # CLI de diagnóstico de PDFs
├── tests/optimizer.rs    # testes de integração
└── .github/workflows/pages.yml
```

## Limitações conhecidas

- **PDFs criptografados** não são suportados (retornam erro).
- **Fontes**: o subconjunto é feito apenas para fontes TrueType (`FontFile2`). Fontes Type1 e CFF são mantidas como estão.
- **Imagens CMYK, JBIG2 e de 1 bit** dentro de PDFs não são recomprimidas.
- **Acessibilidade**: com a opção de remover tags ligada (padrão), leitores de tela perdem a estrutura do documento. Desligue-a se isso for importante.
- **Compressão máxima** roda em uma única thread no navegador (cerca de 40 s para o PDF de 132 páginas). Pela linha de comando, em paralelo, leva poucos segundos.
- **Memória**: o WebAssembly de 32 bits tem até 4 GB; arquivos muito grandes podem falhar no navegador. Nesse caso, a interface mostra o erro e segue para o próximo arquivo.

## Tecnologias

| | |
|---|---|
| Linguagem | Rust (edition 2024) |
| WebAssembly | [`wasm-bindgen`](https://crates.io/crates/wasm-bindgen), [`wasm-pack`](https://github.com/rustwasm/wasm-pack) |
| Imagens | [`image`](https://crates.io/crates/image) |
| PDF | [`lopdf`](https://crates.io/crates/lopdf) (leitura e manipulação) + gravador próprio |
| Fontes | [`ttf-parser`](https://crates.io/crates/ttf-parser) (tabela `cmap`) + subsetter próprio |
| Compressão | [`miniz_oxide`](https://crates.io/crates/miniz_oxide), [`zopfli`](https://crates.io/crates/zopfli) |
| Paralelismo (nativo) | [`rayon`](https://crates.io/crates/rayon) |
| Interface | HTML, CSS e JavaScript puros, com Web Worker |
