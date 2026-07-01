"""
Scorer: wraps an ONNX sentence-importance model.

Heavy imports (onnxruntime, tokenizers, numpy) are loaded LAZILY inside
__init__ so that `import model` succeeds in a bare venv (e.g. during tests).
"""
from __future__ import annotations

import os


class Scorer:
    """Load and run the kompress-v2-base ONNX model."""

    def __init__(self, model_dir: str) -> None:
        # Lazy imports — absent at test time but present in the runtime image.
        import numpy as np  # noqa: F401  (kept for type hints below)
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
        self._tokenizer.enable_padding()
        self._tokenizer.enable_truncation(max_length=512)
        self._np = np

        if os.path.isfile(revision_path):
            with open(revision_path, encoding="utf-8") as fh:
                self._revision = fh.read().strip()
        else:
            self._revision = "unknown"

        # Grab the name of the first output (the logit column we'll sigmoid).
        self._output_name = self._session.get_outputs()[0].name
        self._input_names = [inp.name for inp in self._session.get_inputs()]

    @property
    def revision(self) -> str:
        return self._revision

    def score_batches(self, batches: list[list[str]]) -> list[list[float]]:
        """
        Return one list of floats per input batch, one float per sentence.

        Each float is in [0, 1] (sigmoid of the importance logit).

        We process sentence-by-sentence for simplicity and correctness.
        Batching all sentences in one ONNX call is a straightforward
        optimisation left for a follow-up (pad to same length, stack arrays).
        """
        np = self._np
        results: list[list[float]] = []

        for batch in batches:
            if not batch:
                results.append([])
                continue

            scores: list[float] = []
            for sentence in batch:
                enc = self._tokenizer.encode(sentence)

                # Build the feed dict from whatever inputs the model declares.
                feed: dict[str, object] = {}
                for name in self._input_names:
                    if "input_ids" in name:
                        feed[name] = np.array([enc.ids], dtype=np.int64)
                    elif "attention_mask" in name:
                        feed[name] = np.array([enc.attention_mask], dtype=np.int64)
                    elif "token_type_ids" in name:
                        feed[name] = np.zeros((1, len(enc.ids)), dtype=np.int64)

                logits = self._session.run([self._output_name], feed)[0]
                # logits shape: (1, 1) or (1,) — flatten to scalar
                raw = float(np.asarray(logits).ravel()[0])
                # Sigmoid → score in [0, 1]
                score = float(1.0 / (1.0 + np.exp(-raw)))
                scores.append(score)

            results.append(scores)

        return results
