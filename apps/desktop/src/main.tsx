import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

// StrictMode double-invokes effects, which would register the daemon
// subscription twice. The popup is a single long-lived window, so the extra
// checking is not worth the duplicated listeners.
ReactDOM.createRoot(document.getElementById("root")!).render(<App />);

// Never let the popup show a browser context menu or drag itself around.
document.addEventListener("contextmenu", (e) => e.preventDefault());
document.addEventListener("dragover", (e) => e.preventDefault());
document.addEventListener("drop", (e) => e.preventDefault());

export {};
void React;
