"""
compressor scoring sidecar — FastAPI application.

POST /score   — score sentence batches
GET  /health  — liveness + revision

Inference runs at most `COMPRESSOR_CONCURRENCY` requests at a time (default 1:
one inference already uses every thread the pod has). Up to
`COMPRESSOR_MAX_WAITING` more wait for a turn; past that a request is answered
503 at once. A request carrying `x-obleth-timeout-ms` (the gateway's timeout)
waits and runs only within it, and is answered 504 once it has passed, so work
the gateway has abandoned does not keep the CPU busy. The gateway treats both
answers as "not scored" and compacts by its heuristic instead.
"""
from __future__ import annotations

import logging
import os
import threading
import time
from contextlib import asynccontextmanager
from typing import Optional

from fastapi import FastAPI, Header, HTTPException
from pydantic import BaseModel

from model import DeadlineExceeded

log = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Pydantic models
# ---------------------------------------------------------------------------


class Segment(BaseModel):
    sentences: list[str]


class ScoreRequest(BaseModel):
    segments: list[Segment]


class SegmentResult(BaseModel):
    scores: list[float]


class ScoreResponse(BaseModel):
    results: list[SegmentResult]


class HealthResponse(BaseModel):
    status: str
    model: str
    revision: str


# ---------------------------------------------------------------------------
# Admission
# ---------------------------------------------------------------------------


class Gate:
    """At most `running` inferences at once, and at most `waiting` more queued."""

    def __init__(self, running: int, waiting: int) -> None:
        self._slots = threading.BoundedSemaphore(max(1, running))
        self._max_waiting = max(0, waiting)
        self._waiting = 0
        self._lock = threading.Lock()

    def enter(self, timeout: Optional[float]) -> bool:
        """Take a slot, waiting up to `timeout` seconds (no limit when None).
        False when the queue is full or the wait ran out."""
        if self._slots.acquire(blocking=False):
            return True
        if timeout is not None and timeout <= 0:
            return False
        with self._lock:
            if self._waiting >= self._max_waiting:
                return False
            self._waiting += 1
        try:
            return self._slots.acquire(timeout=timeout)
        finally:
            with self._lock:
                self._waiting -= 1

    def leave(self) -> None:
        self._slots.release()


gate = Gate(
    int(os.environ.get("COMPRESSOR_CONCURRENCY", "1")),
    int(os.environ.get("COMPRESSOR_MAX_WAITING", "8")),
)

# ---------------------------------------------------------------------------
# App + scorer lifecycle
# ---------------------------------------------------------------------------

# Module-level scorer — None until startup succeeds or tests inject a fake.
# Tests can do:  import app; app.scorer = FakeScorer()
scorer: Optional[object] = None  # type: ignore[type-arg]


@asynccontextmanager
async def lifespan(_app: FastAPI):
    """Load the scorer on startup; degrade to None (503 on /score) if absent."""
    global scorer
    model_dir = os.environ.get("COMPRESSOR_MODEL_DIR", "/models")
    try:
        from model import Scorer  # noqa: PLC0415

        scorer = Scorer(model_dir)
        log.info("Scorer loaded from %s (revision=%s)", model_dir, scorer.revision)  # type: ignore[union-attr]
    except Exception as exc:  # noqa: BLE001
        log.warning("Scorer unavailable (%s). /score will return 503.", exc)
        scorer = None
    yield


app = FastAPI(title="compressor", version="0.1.0", lifespan=lifespan)


# ---------------------------------------------------------------------------
# Routes
# ---------------------------------------------------------------------------


@app.get("/health", response_model=HealthResponse)
def health() -> HealthResponse:
    rev = scorer.revision if scorer is not None else "unknown"  # type: ignore[union-attr]
    model = os.environ.get("COMPRESSOR_MODEL_NAME", "compressor")
    return HealthResponse(status="ok", model=model, revision=rev)


@app.post("/score", response_model=ScoreResponse)
def score(
    body: ScoreRequest,
    x_obleth_timeout_ms: Optional[int] = Header(default=None),
) -> ScoreResponse:
    arrived = time.monotonic()
    if scorer is None:
        raise HTTPException(status_code=503, detail="Model not loaded")

    if not body.segments:
        return ScoreResponse(results=[])

    deadline = None
    if x_obleth_timeout_ms is not None and x_obleth_timeout_ms > 0:
        deadline = arrived + x_obleth_timeout_ms / 1000.0
    wait = None if deadline is None else deadline - time.monotonic()
    if not gate.enter(wait):
        raise HTTPException(status_code=503, detail="busy")
    try:
        batches = [seg.sentences for seg in body.segments]
        batch_scores = scorer.score_batches(batches, deadline)  # type: ignore[union-attr]
    except DeadlineExceeded:
        raise HTTPException(status_code=504, detail="deadline passed") from None
    finally:
        gate.leave()

    results = [SegmentResult(scores=s) for s in batch_scores]
    return ScoreResponse(results=results)
