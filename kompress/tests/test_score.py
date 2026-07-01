"""
Unit tests for kompress scoring sidecar.

Uses a monkeypatched FakeScorer — no model download, no onnxruntime required.
"""
import pytest
from fastapi.testclient import TestClient


class FakeScorer:
    """Deterministic scorer for tests: score = min(len(sentence) / 100, 1.0)."""

    @property
    def revision(self) -> str:
        return "test-rev"

    def score_batches(self, batches: list[list[str]]) -> list[list[float]]:
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
    assert body["model"] == "kompress-v2-base"


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
