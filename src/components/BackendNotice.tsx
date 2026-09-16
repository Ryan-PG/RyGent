import { BACKEND_UNAVAILABLE_MESSAGE } from "../services/backend";

interface Props {
  /** Override the default explanation (for example when an action failed). */
  message?: string;
}

/**
 * Shown when the React UI runs without the Rust core - `npm run dev` in a
 * browser, or a preview build. Provider management needs the backend, so the
 * panel says so plainly instead of throwing on `invoke`.
 */
export default function BackendNotice({ message }: Props) {
  return (
    <div className="notice" role="status" aria-label="Backend unavailable">
      <strong>Backend unavailable (run via npm run tauri dev)</strong>
      <p className="muted">{message ?? BACKEND_UNAVAILABLE_MESSAGE}</p>
    </div>
  );
}
