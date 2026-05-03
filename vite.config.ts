import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Vite config for Tauri v2: fixed dev port so tauri.conf.json can point at it,
// and src-tauri excluded from Vite's watch loop to avoid rebuild storms.
//
// Port: 14210 statt Tauris Default 1420, damit `pnpm tauri dev` nicht mit
// einer parallel laufenden anderen Tauri-App kollidiert (Mila etc. nutzen
// den Default). `strictPort: true` lässt Vite hart abbrechen statt
// auf 14211 auszuweichen — sonst zeigt Tauri auf den falschen Port.
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 14210,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
}));
