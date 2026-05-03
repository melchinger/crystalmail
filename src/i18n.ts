import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import de from "./locales/de.json";

// Single-locale start (DE). English can be added by dropping a sibling JSON
// into ./locales/ and listing it under `resources` — no code changes needed.
void i18n.use(initReactI18next).init({
  resources: {
    de: { translation: de },
  },
  lng: "de",
  fallbackLng: "de",
  interpolation: { escapeValue: false },
});

export default i18n;
