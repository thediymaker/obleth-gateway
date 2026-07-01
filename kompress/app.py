"""
kompress scoring sidecar — FastAPI application.

POST /score   — score sentence batches
GET  /health  — liveness + revision
"""
from __future__ import annotations

import logging
import os
from typing import Optional

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

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
# App + scorer lifecycle
# ---------------------------------------------------------------------------

app = FastAPI(title="kompress", version="0.1.0")

# Module-level scorer — None until startup succeeds or tests inject a fake.
# Tests can do:  import app; app.scorer = FakeScorer()
scorer: Optional[object] = None  # type: ignore[type-arg]


def get_scorer():
    """Return the active scorer or None."""
    return scorer


@app.on_event("startup")
def _startup() -> None:
    global scorer
    model_dir = os.environ.get("KOMPRESS_MODEL_DIR", "/models")
    try:
        from model import Scorer  # noqa: PLC0415

        scorer = Scorer(model_dir)
        log.info("Scorer loaded from %s (revision=%s)", model_dir, scorer.revision)  # type: ignore[union-attr]
    except Exception as exc:  # noqa: BLE001
        log.warning("Scorer unavailable (%s). /score will return 503.", exc)
        scorer = None


# ---------------------------------------------------------------------------
# Routes
# ---------------------------------------------------------------------------


@app.get("/health", response_model=HealthResponse)
def health() -> HealthResponse:
    rev = scorer.revision if scorer is not None else "unknown"  # type: ignore[union-attr]
    return HealthResponse(status="ok", model="kompress-v2-base", revision=rev)


@app.post("/score", response_model=ScoreResponse)
def score(body: ScoreRequest) -> ScoreResponse:
    if scorer is None:
        raise HTTPException(status_code=503, detail="Model not loaded")

    if not body.segments:
        return ScoreResponse(results=[])

    batches = [seg.sentences for seg in body.segments]
    batch_scores = scorer.score_batches(batches)  # type: ignore[union-attr]

    results = [SegmentResult(scores=s) for s in batch_scores]
    return ScoreResponse(results=results)
