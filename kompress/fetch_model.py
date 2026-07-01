"""
Build-time model provisioning for the kompress sidecar.

Two sources, selected by --source (KOMPRESS_SOURCE build arg):

  onnx    (default) Download a PRE-BUILT ONNX file + tokenizer from the repo.
          Used by chopratejas/kompress-v2-base, which ships ONNX. Only dep:
          huggingface_hub.

  export  Export a standard AutoModelForTokenClassification model to ONNX at
          build time. Used for models that ship only weights — e.g. LLMLingua-2
          (microsoft/llmlingua-2-*). Deps: torch (CPU) + transformers + onnx.

Either way the runtime reads model.onnx + tokenizer.json + revision.txt from the
output dir via onnxruntime + tokenizers; the runtime scorer is model-agnostic (it
consumes per-token scores/logits). Run ONCE at image build time.

Usage:
    python fetch_model.py --source onnx   --model-id ID --onnx-file PATH --out-dir DIR
    python fetch_model.py --source export --model-id ID                  --out-dir DIR
"""
from __future__ import annotations

import argparse
import os
import shutil


def _write_revision(model_id: str, out_dir: str) -> str:
    revision = "unknown"
    try:
        from huggingface_hub import model_info  # type: ignore[import-untyped]

        revision = model_info(model_id).sha or "unknown"
    except Exception:  # noqa: BLE001
        pass
    with open(os.path.join(out_dir, "revision.txt"), "w", encoding="utf-8") as fh:
        fh.write(revision)
    return revision


def fetch_onnx(model_id: str, out_dir: str, onnx_file: str) -> None:
    """Download a pre-built ONNX artefact + tokenizer from the model repo."""
    try:
        from huggingface_hub import hf_hub_download  # type: ignore[import-untyped]
    except ImportError as exc:
        raise SystemExit(
            f"Build-time dep missing: {exc}\nRun:  pip install huggingface_hub"
        ) from exc

    print(f"Fetching pre-built ONNX {model_id!r} ({onnx_file}) -> {out_dir!r} ...")
    shutil.copyfile(hf_hub_download(repo_id=model_id, filename=onnx_file),
                    os.path.join(out_dir, "model.onnx"))
    shutil.copyfile(hf_hub_download(repo_id=model_id, filename="tokenizer.json"),
                    os.path.join(out_dir, "tokenizer.json"))


def fetch_export(model_id: str, out_dir: str) -> None:
    """Export a token-classification model (e.g. LLMLingua-2) to ONNX via torch."""
    try:
        import torch
        from transformers import AutoModelForTokenClassification, AutoTokenizer
    except ImportError as exc:
        raise SystemExit(
            f"Build-time deps missing: {exc}\n"
            'Run:  pip install "transformers>=4.48,<5" torch onnx'
        ) from exc

    print(f"Exporting token-classification model {model_id!r} -> {out_dir!r} ...")
    model = AutoModelForTokenClassification.from_pretrained(model_id)
    model.eval()
    tokenizer = AutoTokenizer.from_pretrained(model_id)

    class LogitsOnly(torch.nn.Module):
        def __init__(self, inner: torch.nn.Module) -> None:
            super().__init__()
            self.inner = inner

        def forward(self, input_ids, attention_mask):  # noqa: ANN001
            return self.inner(input_ids=input_ids, attention_mask=attention_mask).logits

    enc = tokenizer("The quick brown fox jumps over the lazy dog.", return_tensors="pt")
    with torch.no_grad():
        torch.onnx.export(
            LogitsOnly(model),
            (enc["input_ids"], enc["attention_mask"]),
            os.path.join(out_dir, "model.onnx"),
            input_names=["input_ids", "attention_mask"],
            output_names=["logits"],
            dynamic_axes={
                "input_ids": {0: "batch", 1: "sequence"},
                "attention_mask": {0: "batch", 1: "sequence"},
                "logits": {0: "batch", 1: "sequence"},
            },
            opset_version=17,
            do_constant_folding=True,
        )
    tokenizer.save_pretrained(out_dir)
    if not os.path.isfile(os.path.join(out_dir, "tokenizer.json")):
        raise SystemExit("tokenizer.json not produced — the model needs a fast tokenizer.")


def main() -> None:
    parser = argparse.ArgumentParser(description="Provision the kompress model.")
    parser.add_argument("--source", choices=["onnx", "export"], default="onnx")
    parser.add_argument("--model-id", default="chopratejas/kompress-v2-base")
    parser.add_argument("--out-dir", default="/models")
    parser.add_argument("--onnx-file", default="onnx/kompress-fp32.onnx",
                        help="repo path of the pre-built ONNX (source=onnx only)")
    args = parser.parse_args()

    os.makedirs(args.out_dir, exist_ok=True)
    if args.source == "onnx":
        fetch_onnx(args.model_id, args.out_dir, args.onnx_file)
    else:
        fetch_export(args.model_id, args.out_dir)

    revision = _write_revision(args.model_id, args.out_dir)
    print(f"Done.  Revision: {revision}")
    print(f"Files in {args.out_dir}: {sorted(os.listdir(args.out_dir))}")


if __name__ == "__main__":
    main()
