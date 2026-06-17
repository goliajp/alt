import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Vite config — tailwindcss v4 plugin + react. Dev server proxies the API
// hits to the local alt-web binary so `npm run dev` matches the prod
// shape: /api/* → alt-web JSON, /alt/* → altd-server wire, / → the SPA.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      "/api": {
        target: process.env.VITE_API_TARGET ?? "https://alt.golia.jp",
        changeOrigin: true,
        secure: true,
      },
      "/alt": {
        target: process.env.VITE_API_TARGET ?? "https://alt.golia.jp",
        changeOrigin: true,
        secure: true,
      },
    },
  },
});
