"""
Scorer: wraps the kompress-v2-base ONNX model.

The model (HeadroomCompressorV2, a ModernBERT-based token compressor) emits a
keep-probability in [0, 1] for each subword token. The gateway asks this sidecar
for one importance score PER SENTENCE, so we mean-pool each sentence's content
tokens (excluding special and padding tokens): a sentence whose tokens the
compressor wants to keep is information-dense and should rank higher.

Sentences are scored in BATCHES — all sentences in a chunk are padded to a common
length and run through the model in a single `session.run`, which lets onnxruntime
parallelize across the batch dimension instead of one inference per sentence.
Chunk size is bounded (`COMPRESSOR_MAX_BATCH`) so a huge segment can't build one
giant padded tensor.

Heavy imports (onnxruntime, tokenizers, numpy) are loaded LAZILY inside __init__
so that `import model` succeeds in a bare venv (e.g. during tests).
"""
from __future__ import annotations

import os


class Scorer:
    """Load and run the kompress-v2-base ONNX model."""

    def __init__(self, model_dir: str) -> None:
        # Lazy imports — absent at test time but present in the runtime image.
        import numpy as np
        import onnxruntime as ort
        from tokenizers import Tokenizer  # type: ignore[import-untyped]

        model_path = os.path.join(model_dir, "model.onnx")
        tokenizer_path = os.path.join(model_dir, "tokenizer.json")
        revision_path = os.path.join(model_dir, "revision.txt")

        self._session = ort.InferenceSession(
            model_path,
            providers=["CPUExecutionProvider"],
        )
        self._tokenizer = Tokenizer.from_file(tokenizer_path)
        self._tokenizer.enable_truncation(max_length=512)
        # Pad each batch to its longest member so encodings stack into one tensor.
        # Pad positions carry attention_mask=0, so the pad id never affects scores.
        self._tokenizer.enable_padding()
        self._np = np

        if os.path.isfile(revision_path):
            with open(revision_path, encoding="utf-8") as fh:
                self._revision = fh.read().strip()
        else:
            self._revision = "unknown"

        self._input_names = [inp.name for inp in self._session.get_inputs()]
        # Prefer the per-token keep-probability output ("...scores..."); fall
        # back to the first output if the graph doesn't name it that way.
        outputs = self._session.get_outputs()
        scored = [o.name for o in outputs if "score" in o.name.lower()]
        self._output_name = scored[0] if scored else outputs[0].name

        # Cap sentences per ONNX call so an enormous segment can't allocate one
        # (huge_N x max_len) tensor. Tunable for perf/memory experiments.
        self._max_batch = max(1, int(os.environ.get("COMPRESSOR_MAX_BATCH", "32")))

    @property
    def revision(self) -> str:
        return self._revision

    def _score_chunk(self, sentences: list[str]) -> list[float]:
        """Score up to `_max_batch` sentences in one padded ONNX call."""
        np = self._np
        encs = self._tokenizer.encode_batch(sentences)
        ids = np.array([e.ids for e in encs], dtype=np.int64)
        mask = np.array([e.attention_mask for e in encs], dtype=np.int64)

        feed: dict[str, object] = {}
        for name in self._input_names:
            if "input_ids" in name:
                feed[name] = ids
            elif "attention_mask" in name:
                feed[name] = mask
            elif "token_type_ids" in name:
                feed[name] = np.zeros_like(ids)

        out = np.asarray(self._session.run([self._output_name], feed)[0])
        # out shape: (B, L) per-token score, or (B, L, C) per-token class logits.
        if out.ndim == 3:
            if out.shape[-1] == 1:
                per_token = out[:, :, 0]
            else:
                shifted = out - out.max(axis=-1, keepdims=True)
                exp = np.exp(shifted)
                per_token = (exp / exp.sum(axis=-1, keepdims=True))[:, :, -1]
        else:
            per_token = out  # (B, L)

        # Guard: the shipped ONNX outputs [0, 1] already; if a variant ever emits
        # raw logits, squash them so the per-sentence mean stays a probability.
        if per_token.size and (per_token.min() < 0.0 or per_token.max() > 1.0):
            per_token = 1.0 / (1.0 + np.exp(-per_token))

        scores: list[float] = []
        for i, enc in enumerate(encs):
            attended = np.asarray(enc.attention_mask, dtype=bool)
            special = np.asarray(enc.special_tokens_mask, dtype=bool)
            content = attended & ~special
            row = per_token[i]
            if content.any():
                vals = row[content]
            elif attended.any():
                vals = row[attended]
            else:
                vals = row
            scores.append(float(vals.mean()) if vals.size else 0.0)
        return scores

    def score_batches(self, batches: list[list[str]]) -> list[list[float]]:
        """
        Return one list of floats per input batch, one float per sentence.

        Each float is a per-sentence importance in [0, 1] — the mean keep
        probability the token compressor assigns to that sentence's content
        tokens. Each batch is scored in `_max_batch`-sized ONNX calls.
        """
        results: list[list[float]] = []
        for batch in batches:
            if not batch:
                results.append([])
                continue
            scores: list[float] = []
            for start in range(0, len(batch), self._max_batch):
                scores.extend(self._score_chunk(batch[start : start + self._max_batch]))
            results.append(scores)
        return results
