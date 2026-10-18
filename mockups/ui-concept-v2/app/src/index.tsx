import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles.css";
import { WorkspaceProvider } from './app/workspace';

createRoot(document.getElementById("root")!).render(<WorkspaceProvider><App /></WorkspaceProvider>);
