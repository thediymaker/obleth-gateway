"""
Build-time fetch script: downloads the PRE-BUILT ONNX artefact + tokenizer for
chopratejas/kompress-v2-base into the directory the sidecar serves.

The model repo already ships ONNX (`onnx/kompress-fp32.onnx`), so there is no
torch/transformers export step — we just pull the files. Run this ONCE at Docker
image build time. It needs one build-time dependency NOT in requirements.txt:

    pip install huggingface_hub

The runtime sidecar reads `model.onnx` + `tokenizer.json` + `revision.txt` from
the output dir via onnxruntime + tokenizers — no torch/transformers at run time.

Usage:
    python fetch_model.py [--model-id ID] [--out-dir DIR] [--onnx-file PATH]

Defaults:
    --model-id   chopratejas/kompress-v2-base
    --out-dir    /models
    --onnx-file  onnx/kompress-fp32.onnx   (use onnx/kompress-int8-wo.onnx for a
                                            smaller, faster, slightly-lossier build)
"""
from __future__ import annotations

import argparse
import os
import shutil


def fetch(model_id: str, out_dir: str, onnx_file: str) -> None:
    try:
        from huggingface_hub import hf_hub_download, model_info  # type: ignore[import-untyped]
    except ImportError as exc:
        raise SystemExit(
            f"Build-time dep missing: {exc}\nRun:  pip install huggingface_hub"
        ) from exc

    os.makedirs(out_dir, exist_ok=True)
    print(f"Fetching {model_id!r} ({onnx_file}) -> {out_dir!r} ...")

    onnx_src = hf_hub_download(repo_id=model_id, filename=onnx_file)
    shutil.copyfile(onnx_src, os.path.join(out_dir, "model.onnx"))

    tokenizer_src = hf_hub_download(repo_id=model_id, filename="tokenizer.json")
    shutil.copyfile(tokenizer_src, os.path.join(out_dir, "tokenizer.json"))

    # Resolve the commit revision for /health provenance.
    revision = "unknown"
    try:
        revision = model_info(model_id).sha or "unknown"
    except Exception:  # noqa: BLE001
        pass

    with open(os.path.join(out_dir, "revision.txt"), "w", encoding="utf-8") as fh:
        fh.write(revision)

    print(f"Fetch complete.  Revision: {revision}")
    print(f"Files in {out_dir}: {sorted(os.listdir(out_dir))}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Fetch the kompress ONNX artefact.")
    parser.add_argument(
        "--model-id",
        default="chopratejas/kompress-v2-base",
        help="HuggingFace model id (default: chopratejas/kompress-v2-base)",
    )
    parser.add_argument(
        "--out-dir",
        default="/models",
        help="Output directory for the ONNX artefacts (default: /models)",
    )
    parser.add_argument(
        "--onnx-file",
        default="onnx/kompress-fp32.onnx",
        help="Which ONNX file in the repo to fetch (default: onnx/kompress-fp32.onnx)",
    )
    args = parser.parse_args()
    fetch(args.model_id, args.out_dir, args.onnx_file)


if __name__ == "__main__":
    main()
