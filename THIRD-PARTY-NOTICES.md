# Third-Party Notices

SnipStack bundles or downloads the following third-party components beyond
its Rust/npm dependencies (whose licenses are declared in `Cargo.toml` /
`package.json` metadata).

## EcoPaste (upstream project)

SnipStack is a hard fork of [EcoPaste](https://github.com/EcoPasteHub/EcoPaste)
by ayangweb and contributors, licensed under the Apache License 2.0.
See [NOTICE](./NOTICE) for the required attribution.

## PP-OCRv5 models (bundled and downloadable language packs)

- Source models: [PaddleOCR / PP-OCRv5](https://github.com/PaddlePaddle/PaddleOCR),
  Copyright (c) PaddlePaddle Authors, licensed under the Apache License 2.0.
- ONNX conversions are obtained from the
  [oar-ocr](https://github.com/GreatV/oar-ocr) project's model releases
  (Apache License 2.0) and verified by SHA-256 at download time.
- The bundled detection/recognition models (~21 MB) ship inside the
  installer under `resources/ocr/`; optional language packs (Korean,
  Latin-script, East Slavic, Thai, Arabic) are downloaded in-app from the
  same sources.

## ONNX Runtime

OCR inference uses [ONNX Runtime](https://github.com/microsoft/onnxruntime)
(MIT License), statically linked via the [ort](https://github.com/pykeio/ort)
crate.

## rxing

QR/barcode decoding uses [rxing](https://github.com/rxing-core/rxing)
(Apache License 2.0), a Rust port of the ZXing library.

## Microsoft Visual C++ Runtime

The Windows installer deploys the Microsoft Visual C++ 2015–2022
Redistributable runtime DLLs (`msvcp140*.dll`, `vcruntime140*.dll`,
`concrt140.dll`, `vccorlib140.dll`) next to `SnipStack.exe` ("app-local"
deployment) because the statically linked ONNX Runtime depends on the C++
standard library. These files are redistributed under the terms of the
Microsoft Visual Studio redistributable license (see Microsoft's
`Redist.txt` for Visual Studio 2022). They are not modified.
