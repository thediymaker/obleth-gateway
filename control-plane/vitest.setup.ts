// jsdom implements the scroll *properties* (scrollTop, scrollHeight) but not
// the scroll *methods*, so any component that calls `el.scrollTo(...)` in an
// effect throws on mount under test — which is a property of the environment,
// not of the component. Several timelines here keep themselves pinned to the
// newest message that way (workspaces.tsx, image-workspace.tsx,
// charo-panel.tsx), so stub it centrally rather than reshaping each of them
// around the test runner.
//
// The stub moves scrollTop so assertions about scroll position still mean
// something; jsdom has no layout, so scrollHeight is 0 and nothing observable
// changes in practice.
if (typeof Element !== "undefined" && !Element.prototype.scrollTo) {
  Element.prototype.scrollTo = function scrollTo(
    options?: ScrollToOptions | number,
    y?: number,
  ): void {
    const top = typeof options === "number" ? y : options?.top;
    const left = typeof options === "number" ? options : options?.left;
    if (typeof top === "number") this.scrollTop = top;
    if (typeof left === "number") this.scrollLeft = left;
  };
}
