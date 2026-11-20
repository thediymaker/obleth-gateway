"""
Scorer: wraps the kompress-v2-base ONNX model.

The model (HeadroomCompressorV2, a ModernBERT-based token compressor) emits a
keep-probability in [0, 1] for each subword token. The gateway asks this sidecar
for one importance score PER SENTENCE, so we mean-pool each sentence's content
tokens (excluding special and padding tokens): a sentence whose tokens the
compressor wants to keep is information-dense and should rank higher.

A request's sentences, across all of its segments, are tokenized once, sorted by
token count and scored in chunks of `COMPRESSOR_MAX_BATCH`: each chunk is padded
to its longest member and run in a single `session.run`, so short sentences are
not padded out to a long one and a many-segment request makes few ONNX calls.
Scores go back to each sentence's segment and position. Between chunks the
scorer checks the caller's deadline and gives up once it has passed: the
gateway has stopped waiting by then.

Heavy imports (onnxruntime, tokenizers, numpy) are loaded LAZILY inside __init__
so that `import model` succeeds in a bare venv (e.g. during tests).
"""
from __future__ import annotations

import os
import time
from typing import Optional, Sequence


class DeadlineExceeded(Exception):
    """The caller's deadline passed before every sentence was scored."""


def chunk_order(lengths: Sequence[int], max_batch: int) -> list[list[int]]:
    """
    Indices of `lengths`, shortest first, cut into chunks of at most
    `max_batch`. Ties keep their original order, so the plan is deterministic.
    """
    order = sorted(range(len(lengths)), key=lambda i: (lengths[i], i))
    return [order[i : i + max_batch] for i in range(0, len(order), max_batch)]


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

        # onnxruntime otherwise spins up one intra-op thread per host core and
        # saturates the whole machine on a single inference (throughput is
        # CPU-bound and flat with request size). Bound it to COMPRESSOR_NUM_THREADS
        # so one sidecar stays within its allotment — the deploy sets this to the
        # pod/container CPU limit. Unset = onnxruntime default (all cores).
        sess_options = ort.SessionOptions()
        num_threads = os.environ.get("COMPRESSOR_NUM_THREADS")
        if num_threads:
            n = max(1, int(num_threads))
            sess_options.intra_op_num_threads = n
            sess_options.inter_op_num_threads = 1
        self._session = ort.InferenceSession(
            model_path,
            sess_options,
            providers=["CPUExecutionProvider"],
        )
        tokenizer = Tokenizer.from_file(tokenizer_path)
        tokenizer.enable_truncation(max_length=512)
        # Padding is done per chunk (see `_score_encoded`) with the tokenizer's
        # own pad id. Pad positions carry attention_mask=0, so the pad id never
        # affects scores.
        tokenizer.enable_padding()
        self._pad_id = int(tokenizer.padding["pad_id"])
        tokenizer.no_padding()
        self._tokenizer = tokenizer
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

        # Cap sentences per ONNX call so an enormous request can't allocate one
        # (huge_N x max_len) tensor. Tunable for perf/memory experiments.
        self._max_batch = max(1, int(os.environ.get("COMPRESSOR_MAX_BATCH", "8")))

    @property
    def revision(self) -> str:
        return self._revision

    def _score_encoded(self, encs: list) -> list[float]:
        """Score tokenized sentences in one ONNX call, padded to the longest."""
        np = self._np
        width = max(len(e.ids) for e in encs)
        ids = np.full((len(encs), width), self._pad_id, dtype=np.int64)
        mask = np.zeros((len(encs), width), dtype=np.int64)
        content = np.zeros((len(encs), width), dtype=bool)
        for i, e in enumerate(encs):
            n = len(e.ids)
            ids[i, :n] = e.ids
            mask[i, :n] = e.attention_mask
            content[i, :n] = np.asarray(e.attention_mask, dtype=bool) & ~np.asarray(
                e.special_tokens_mask, dtype=bool
            )

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
        for i, e in enumerate(encs):
            row = per_token[i]
            if content[i].any():
                vals = row[content[i]]
            elif mask[i].any():
                vals = row[mask[i].astype(bool)]
            else:
                vals = row[: len(e.ids)]
            scores.append(float(vals.mean()) if vals.size else 0.0)
        return scores

    def score_batches(
        self, batches: list[list[str]], deadline: Optional[float] = None
    ) -> list[list[float]]:
        """
        Return one list of floats per input batch, one float per sentence.

        Each float is a per-sentence importance in [0, 1] — the mean keep
        probability the token compressor assigns to that sentence's content
        tokens. `deadline` is a `time.monotonic()` value: once it has passed,
        no further chunk is started and `DeadlineExceeded` is raised.
        """
        where = [(seg, pos) for seg, batch in enumerate(batches) for pos in range(len(batch))]
        results: list[list[float]] = [[0.0] * len(batch) for batch in batches]
        if not where:
            return results
        encs = self._tokenizer.encode_batch([s for batch in batches for s in batch])
        for chunk in chunk_order([len(e.ids) for e in encs], self._max_batch):
            if deadline is not None and time.monotonic() >= deadline:
                raise DeadlineExceeded()
            for k, score in zip(chunk, self._score_encoded([encs[k] for k in chunk])):
                seg, pos = where[k]
                results[seg][pos] = score
        return results
