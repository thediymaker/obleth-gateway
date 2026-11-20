"""
Unit tests for compressor scoring sidecar.

Uses a monkeypatched FakeScorer — no model download, no onnxruntime required.
"""
import pytest
from fastapi.testclient import TestClient


class FakeScorer:
    """Deterministic scorer for tests: score = min(len(sentence) / 100, 1.0)."""

    @property
    def revision(self) -> str:
        return "test-rev"

    def score_batches(self, batches: list[list[str]], deadline=None) -> list[list[float]]:
        results = []
        for batch in batches:
            scores = [min(len(s) / 100.0, 1.0) for s in batch]
            results.append(scores)
        return results


@pytest.fixture()
def client():
    import app as app_module

    app_module.scorer = FakeScorer()
    return TestClient(app_module.app)


def test_health_ok(client):
    resp = client.get("/health")
    assert resp.status_code == 200
    body = resp.json()
    assert body["status"] == "ok"
    # /health reports COMPRESSOR_MODEL_NAME (baked from the model id at build
    # time); unset in tests, so it falls back to the generic default.
    assert body["model"] == "compressor"


def test_score_aligned(client):
    payload = {
        "segments": [
            {"sentences": ["Hello world.", "A somewhat longer sentence here."]},
            {"sentences": ["Just one."]},
        ]
    }
    resp = client.post("/score", json=payload)
    assert resp.status_code == 200
    body = resp.json()
    assert len(body["results"]) == 2
    assert len(body["results"][0]["scores"]) == 2
    assert len(body["results"][1]["scores"]) == 1


def test_score_empty_segments(client):
    resp = client.post("/score", json={"segments": []})
    assert resp.status_code == 200
    assert resp.json() == {"results": []}


def test_score_empty_sentences(client):
    resp = client.post("/score", json={"segments": [{"sentences": []}]})
    assert resp.status_code == 200
    body = resp.json()
    assert body["results"][0]["scores"] == []


def test_score_scores_in_range(client):
    payload = {
        "segments": [
            {
                "sentences": [
                    "Short.",
                    "A" * 50,
                    "B" * 200,
                ]
            }
        ]
    }
    resp = client.post("/score", json=payload)
    assert resp.status_code == 200
    scores = resp.json()["results"][0]["scores"]
    for score in scores:
        assert 0.0 <= score <= 1.0, f"Score {score} out of [0, 1]"


# ---------------------------------------------------------------------------
# Scheduling across segments (no model: a fake tokenizer and chunk scorer)
# ---------------------------------------------------------------------------


class _Enc:
    def __init__(self, text: str) -> None:
        # One "token" per word, so lengths differ by sentence.
        self.ids = list(range(len(text.split())))


class _Tok:
    def encode_batch(self, texts):
        return [_Enc(t) for t in texts]


def _fake_scorer(max_batch: int):
    from model import Scorer

    s = Scorer.__new__(Scorer)
    s._tokenizer = _Tok()
    s._max_batch = max_batch
    s.calls = []

    def score_encoded(encs):
        s.calls.append([len(e.ids) for e in encs])
        return [len(e.ids) / 100.0 for e in encs]

    s._score_encoded = score_encoded
    return s


def test_chunks_are_cut_from_sentences_sorted_by_length():
    from model import chunk_order

    assert chunk_order([5, 1, 3, 1, 9], 2) == [[1, 3], [2, 0], [4]]
    assert chunk_order([], 4) == []


def test_scores_return_to_their_segment_and_position():
    segments = [
        ["one two three four five", "a"],
        [],
        ["b c", "d e f g h i j k", "l m m"],
    ]
    together = _fake_scorer(max_batch=2)
    merged = together.score_batches(segments)
    one_by_one = [_fake_scorer(max_batch=2).score_batches([seg])[0] for seg in segments]
    assert merged == one_by_one
    # Five sentences in three calls of similar lengths, shortest first.
    assert together.calls == [[1, 2], [3, 5], [8]]


def test_an_expired_deadline_stops_before_the_next_chunk():
    import time

    import pytest

    from model import DeadlineExceeded

    s = _fake_scorer(max_batch=1)
    with pytest.raises(DeadlineExceeded):
        s.score_batches([["a b", "c"]], deadline=time.monotonic() - 1)
    assert s.calls == []


def test_the_gateway_timeout_bounds_the_work(client):
    import app as app_module
    from model import DeadlineExceeded

    class Slow:
        revision = "test-rev"

        def score_batches(self, batches, deadline=None):
            assert deadline is not None, "the header sets a deadline"
            raise DeadlineExceeded()

    app_module.scorer = Slow()
    resp = client.post(
        "/score",
        json={"segments": [{"sentences": ["a"]}]},
        headers={"x-obleth-timeout-ms": "50"},
    )
    assert resp.status_code == 504


def test_a_full_queue_is_answered_busy_at_once(client):
    import app as app_module

    gate = app_module.Gate(running=1, waiting=0)
    assert gate.enter(None)
    assert not gate.enter(None), "no room to wait"
    gate.leave()
    assert gate.enter(0.01)
    gate.leave()

    held = app_module.gate
    app_module.gate = app_module.Gate(running=1, waiting=0)
    try:
        assert app_module.gate.enter(None)
        resp = client.post("/score", json={"segments": [{"sentences": ["a"]}]})
        assert resp.status_code == 503
        assert resp.json()["detail"] == "busy"
    finally:
        app_module.gate.leave()
        app_module.gate = held
