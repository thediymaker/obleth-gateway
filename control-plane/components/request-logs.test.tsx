import { describe, it, expect, afterEach } from "vitest";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { act } from "react";
import { SessionCell } from "./request-logs";
import type { UsageLogEntry } from "@/lib/obleth";

// Minimal UsageLogEntry fixture — only the fields SessionCell uses.
function makeEntry(
  session_id: string,
  session_id_source: string,
): Pick<UsageLogEntry, "session_id" | "session_id_source"> {
  return { session_id, session_id_source };
}

let container: HTMLDivElement;
let root: Root;

function renderCell(entry: Pick<UsageLogEntry, "session_id" | "session_id_source">) {
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
    root.render(<SessionCell entry={entry} />);
  });
}

afterEach(() => {
  act(() => root.unmount());
  document.body.removeChild(container);
});

describe("SessionCell", () => {
  it('renders "derived" badge when session_id_source is "derived"', () => {
    renderCell(makeEntry("sess-abc123", "derived"));
    expect(container.textContent).toContain("derived");
    expect(container.textContent).not.toContain("client");
  });

  it('renders "client" badge when session_id_source is "client"', () => {
    renderCell(makeEntry("sess-xyz789", "client"));
    expect(container.textContent).toContain("client");
    expect(container.textContent).not.toContain("derived");
  });

  it('renders "--" when session_id is empty', () => {
    renderCell(makeEntry("", "none"));
    expect(container.textContent).toContain("--");
    expect(container.textContent).not.toContain("client");
    expect(container.textContent).not.toContain("derived");
  });

  it('renders id with no badge when session_id_source is "none" and session_id is non-empty', () => {
    renderCell(makeEntry("sess-orphan", "none"));
    expect(container.textContent).toContain("sess-orphan");
    expect(container.textContent).not.toContain("client");
    expect(container.textContent).not.toContain("derived");
  });
});
