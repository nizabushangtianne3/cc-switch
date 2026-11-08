import { afterEach, describe, expect, it } from "vitest";
import {
  initializeInputModality,
  isKeyboardModality,
} from "@/lib/inputModality";

const keydown = (key: string, init: KeyboardEventInit = {}) =>
  window.dispatchEvent(new KeyboardEvent("keydown", { key, ...init }));

const pointerdown = () => window.dispatchEvent(new Event("pointerdown"));

afterEach(() => {
  document.documentElement.removeAttribute("data-keyboard");
});

describe("inputModality", () => {
  it("marks keyboard modality on a focus-moving key", () => {
    initializeInputModality();
    expect(isKeyboardModality()).toBe(false);

    keydown("Tab");

    expect(isKeyboardModality()).toBe(true);
    expect(document.documentElement.dataset.keyboard).toBe("");
  });

  it("marks keyboard modality on arrow and page keys", () => {
    initializeInputModality();
    keydown("ArrowDown");
    expect(isKeyboardModality()).toBe(true);
    pointerdown();
    keydown("PageUp");
    expect(isKeyboardModality()).toBe(true);
  });

  it("ignores Enter, Space and Escape so typing does not leave focus rings", () => {
    initializeInputModality();
    for (const key of ["Enter", " ", "Escape"]) keydown(key);

    expect(isKeyboardModality()).toBe(false);
  });

  it("ignores navigation keys combined with modifiers", () => {
    initializeInputModality();
    keydown("Tab", { ctrlKey: true });
    keydown("ArrowLeft", { metaKey: true });
    keydown("Home", { altKey: true });

    expect(isKeyboardModality()).toBe(false);
  });

  it("clears the keyboard marker on the next pointer press", () => {
    initializeInputModality();
    keydown("Tab");
    expect(isKeyboardModality()).toBe(true);

    pointerdown();

    expect(isKeyboardModality()).toBe(false);
    expect(document.documentElement.hasAttribute("data-keyboard")).toBe(false);
  });
});
