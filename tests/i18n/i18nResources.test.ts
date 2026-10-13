import i18n, { i18nReady } from "@/i18n";

describe("i18n resources", () => {
  it("loads only the language requested at runtime", async () => {
    for (const language of ["en", "zh", "zh-TW"] as const) {
      i18n.removeResourceBundle(language, "translation");
    }

    await i18nReady;
    await i18n.changeLanguage("ja");

    expect(i18n.hasResourceBundle("ja", "translation")).toBe(true);
    expect(i18n.hasResourceBundle("zh", "translation")).toBe(false);
    expect(i18n.hasResourceBundle("zh-TW", "translation")).toBe(false);
  });
});
