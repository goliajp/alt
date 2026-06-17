import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Route, Routes } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import "./index.css";
import { Layout } from "./components/Layout";
import { Home } from "./pages/Home";
import { RepoHome } from "./pages/RepoHome";
import { Log } from "./pages/Log";
import { Commit } from "./pages/Commit";
import { Browse } from "./pages/Browse";
import { FileHistory } from "./pages/FileHistory";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      refetchOnWindowFocus: false,
      retry: false,
    },
  },
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <Routes>
          <Route element={<Layout />}>
            <Route index element={<Home />} />
            <Route path="r/:name" element={<RepoHome />} />
            <Route path="r/:name/commits" element={<Log />} />
            <Route path="r/:name/commits/:oid" element={<Commit />} />
            <Route path="r/:name/tree/:spec/*" element={<Browse />} />
            <Route path="r/:name/tree/:spec" element={<Browse />} />
            <Route path="r/:name/history" element={<FileHistory />} />
          </Route>
        </Routes>
      </BrowserRouter>
    </QueryClientProvider>
  </StrictMode>,
);
