"""
Build-time export script: downloads chopratejas/kompress-v2-base from the
Hugging Face Hub and converts it to ONNX for use by the scoring sidecar.

Run this ONCE at Docker image build time.  It requires build-time dependencies
that are NOT in requirements.txt:

    pip install transformers optimum[exporters] torch

Usage:
    python export_model.py [--model-id MODEL_ID] [--out-dir OUT_DIR]

Defaults:
    --model-id  chopratejas/kompress-v2-base
    --out-dir   /models
"""
from __future__ import annotations

import argparse
import os
import shutil


def export(model_id: str, out_dir: str) -> None:
    # Build-time imports — torch / transformers / optimum are not in the
    # runtime requirements.txt.  If they are missing the error message is clear.
    try:
        from optimum.onnxruntime import ORTModelForSequenceClassification  # type: ignore[import-untyped]
        from transformers import AutoTokenizer  # type: ignore[import-untyped]
    except ImportError as exc:
        raise SystemExit(
            f"Build-time deps missing: {exc}\n"
            "Run:  pip install transformers optimum[exporters] torch"
        ) from exc

    os.makedirs(out_dir, exist_ok=True)

    print(f"Exporting {model_id!r} → {out_dir!r} ...")

    # Export the model to ONNX via optimum.
    # `export=True` triggers the ONNX conversion; the result is saved to out_dir
    # as model.onnx (along with the config and tokenizer artefacts).
    #
    # NOTE: if the model's task is not sequence-classification, swap to
    # ORTModelForFeatureExtraction and adjust model.py's logit extraction
    # accordingly.
    model = ORTModelForSequenceClassification.from_pretrained(
        model_id,
        export=True,
    )
    model.save_pretrained(out_dir)

    # Also save the tokenizer's fast JSON file for use by tokenizers.Tokenizer.
    tokenizer = AutoTokenizer.from_pretrained(model_id)
    tokenizer.save_pretrained(out_dir)

    # Resolve the commit revision for health-check provenance.
    try:
        from huggingface_hub import model_info  # type: ignore[import-untyped]

        info = model_info(model_id)
        revision = info.sha or "unknown"
    except Exception:  # noqa: BLE001
        revision = "unknown"

    revision_path = os.path.join(out_dir, "revision.txt")
    with open(revision_path, "w", encoding="utf-8") as fh:
        fh.write(revision)

    # Confirm model.onnx is present (optimum may name it differently).
    onnx_candidates = [f for f in os.listdir(out_dir) if f.endswith(".onnx")]
    if "model.onnx" not in onnx_candidates and onnx_candidates:
        # Rename the first .onnx file to model.onnx so model.py can find it.
        src = os.path.join(out_dir, onnx_candidates[0])
        dst = os.path.join(out_dir, "model.onnx")
        shutil.move(src, dst)
        print(f"Renamed {onnx_candidates[0]} → model.onnx")

    print(f"Export complete.  Revision: {revision}")
    print(f"Files in {out_dir}: {os.listdir(out_dir)}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Export kompress model to ONNX.")
    parser.add_argument(
        "--model-id",
        default="chopratejas/kompress-v2-base",
        help="HuggingFace model id (default: chopratejas/kompress-v2-base)",
    )
    parser.add_argument(
        "--out-dir",
        default="/models",
        help="Output directory for ONNX artefacts (default: /models)",
    )
    args = parser.parse_args()
    export(args.model_id, args.out_dir)


if __name__ == "__main__":
    main()
