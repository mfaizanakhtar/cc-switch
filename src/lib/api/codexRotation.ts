import { invoke } from "@tauri-apps/api/core";
import type { CodexRotationStatus } from "@/types";

export const codexRotationApi = {
  status: (): Promise<CodexRotationStatus> =>
    invoke("get_codex_rotation_status"),
  runNow: (): Promise<CodexRotationStatus> =>
    invoke("run_codex_rotation_now"),
  pause: (paused: boolean): Promise<CodexRotationStatus> =>
    invoke("set_codex_rotation_paused", { paused }),
  selectServingAccount: (
    accountId: string | null,
  ): Promise<CodexRotationStatus> =>
    invoke("set_codex_rotation_serving_account", { accountId }),
  setAnchor: (providerId: string): Promise<CodexRotationStatus> =>
    invoke("set_codex_rotation_anchor", { providerId }),
};
