// @vitest-environment node
import { describe, expect, it } from "vitest";
import { withCurrentCallbackPath } from "./legacy-callback";

describe("withCurrentCallbackPath", () => {
  it("moves a legacy OIDC callback to the current path, query kept", () => {
    const req = withCurrentCallbackPath(
      new Request("https://dash.example.edu/api/auth/oauth2/callback/campus?code=c&state=s"),
    );
    expect(req.url).toBe("https://dash.example.edu/api/auth/callback/campus?code=c&state=s");
  });

  it("keeps the method, headers and body of a form_post callback", async () => {
    const req = withCurrentCallbackPath(
      new Request("https://dash.example.edu/api/auth/oauth2/callback/campus", {
        method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded", cookie: "a=b" },
        body: "code=c&state=s",
      }),
    );
    expect(new URL(req.url).pathname).toBe("/api/auth/callback/campus");
    expect(req.method).toBe("POST");
    expect(req.headers.get("cookie")).toBe("a=b");
    expect(await req.text()).toBe("code=c&state=s");
  });

  it("leaves every other path alone", () => {
    const req = new Request("https://dash.example.edu/api/auth/callback/campus?code=c");
    expect(withCurrentCallbackPath(req)).toBe(req);
  });
});
