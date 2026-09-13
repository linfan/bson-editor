import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export default function App() {
  const [version, setVersion] = useState("");

  useEffect(() => {
    invoke<string>("core_version").then(setVersion).catch(console.error);
  }, []);

  return (
    <main className="app">
      <h1>BSON Editor</h1>
      <p>doccore v{version || "…"}</p>
    </main>
  );
}
