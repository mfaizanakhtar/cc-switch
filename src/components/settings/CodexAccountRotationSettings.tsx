import { useEffect, useMemo, useState } from "react";
import type { SettingsFormState } from "@/hooks/useSettings";
import type { ManagedAuthAccount } from "@/lib/api";
import { authApi, codexRotationApi, providersApi } from "@/lib/api";
import type {
  CodexAccountRotation,
  CodexRotationAccount,
  CodexRotationStatus,
  Provider,
} from "@/types";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/lib/toast";
import { SettingsRow, SettingsSwitchRow } from "./SettingsLayout";

const DEFAULTS: CodexAccountRotation = {
  enabled: false,
  anchorProviderId: "",
  pollSeconds: 120,
  fastPollSeconds: 30,
  nearCapMargin: 10,
  manualOverrideSeconds: 900,
  manualPaused: false,
  accounts: [],
  heartbeat: {
    enabled: false,
    target: "preferred",
    idleMinutes: 30,
    repeatMinutes: 300,
  },
};

function clampPercent(value: string): number {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return 0;
  return Math.max(0, Math.min(100, Math.round(parsed)));
}

function formatTime(ms: number | null | undefined): string {
  if (!ms) return "—";
  return new Date(ms).toLocaleString();
}

function remaining(
  used: number | null | undefined,
  minRemaining: number,
): string {
  if (used == null) return "unknown";
  return `${Math.max(0, 100 - used).toFixed(1)}% left (floor ${minRemaining}%)`;
}

export function CodexAccountRotationSettings({
  settings,
  onChange,
}: {
  settings: SettingsFormState;
  onChange: (
    updates: Partial<SettingsFormState>,
  ) => void | boolean | Promise<void | boolean>;
}) {
  const [draft, setDraft] = useState<CodexAccountRotation>(
    settings.codexAccountRotation ?? DEFAULTS,
  );
  const [status, setStatus] = useState<CodexRotationStatus | null>(null);
  const [managed, setManaged] = useState<ManagedAuthAccount[]>([]);
  const [providers, setProviders] = useState<Provider[]>([]);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    setDraft(settings.codexAccountRotation ?? DEFAULTS);
  }, [settings.codexAccountRotation]);

  useEffect(() => {
    let alive = true;
    const load = () => {
      void codexRotationApi
        .status()
        .then((next) => alive && setStatus(next))
        .catch(() => undefined);
    };
    load();
    const timer = window.setInterval(load, 3000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    void authApi
      .authGetStatus("codex_oauth")
      .then((next) => setManaged(next.accounts))
      .catch(() => undefined);
    void providersApi
      .getAll("codex")
      .then((next) => setProviders(Object.values(next)))
      .catch(() => undefined);
  }, []);

  const anchorOptions = useMemo(
    () =>
      providers.filter(
        (provider) =>
          provider.category === "official" &&
          provider.meta?.authBinding?.source === "managed_account" &&
          !!provider.meta?.authBinding?.accountId,
      ),
    [providers],
  );

  const addable = managed.filter(
    (account) =>
      !draft.accounts.some((rule) => rule.accountId === account.id),
  );

  const update = (patch: Partial<CodexAccountRotation>) =>
    setDraft((current) => ({ ...current, ...patch }));

  const updateAccount = (
    index: number,
    patch: Partial<CodexRotationAccount>,
  ) =>
    setDraft((current) => ({
      ...current,
      accounts: current.accounts.map((account, position) =>
        position === index ? { ...account, ...patch } : account,
      ),
    }));

  const reorder = (index: number, direction: -1 | 1) => {
    const target = index + direction;
    if (target < 0 || target >= draft.accounts.length) return;
    setDraft((current) => {
      const accounts = [...current.accounts];
      [accounts[index], accounts[target]] = [accounts[target], accounts[index]];
      return {
        ...current,
        accounts: accounts.map((account, position) => ({
          ...account,
          priority: position + 1,
        })),
      };
    });
  };

  const addAccount = (accountId: string) => {
    const account = managed.find((item) => item.id === accountId);
    if (!account) return;
    setDraft((current) => ({
      ...current,
      accounts: [
        ...current.accounts,
        {
          accountId: account.id,
          login: account.login,
          priority: current.accounts.length + 1,
          fiveHourMinRemaining: 0,
          weeklyMinRemaining: 0,
          heartbeatEligible: false,
        },
      ],
    }));
  };

  const save = async () => {
    if (draft.enabled && !draft.anchorProviderId) {
      toast.error("Select an anchor card first");
      return;
    }
    setSaving(true);
    try {
      if (draft.anchorProviderId) {
        await codexRotationApi.setAnchor(draft.anchorProviderId);
      }
      const saved = await onChange({ codexAccountRotation: draft });
      if (saved === false) {
        toast.error("Could not save account rotation settings");
      } else {
        toast.success("Account rotation settings saved");
      }
    } catch {
      toast.error("Could not save account rotation settings");
    } finally {
      setSaving(false);
    }
  };

  const runNow = async () => {
    try {
      setStatus(await codexRotationApi.runNow());
    } catch {
      toast.error("Could not run account rotation now");
    }
  };

  const togglePause = async () => {
    const next = !(status?.manualPaused ?? draft.manualPaused);
    try {
      const snapshot = await codexRotationApi.pause(next);
      setStatus(snapshot);
      update({ manualPaused: next });
    } catch {
      toast.error("Could not change the rotation pause state");
    }
  };

  const selectServing = async (accountId: string) => {
    try {
      setStatus(await codexRotationApi.selectServingAccount(accountId || null));
    } catch {
      toast.error("Could not select the serving account");
    }
  };

  const selectAnchor = async (providerId: string) => {
    if (!providerId) return;
    update({ anchorProviderId: providerId });
    try {
      setStatus(await codexRotationApi.setAnchor(providerId));
    } catch {
      toast.error("Could not mark that card as the rotation anchor");
    }
  };

  const statusById = new Map(
    (status?.accounts ?? []).map((account) => [account.accountId, account]),
  );

  return (
    <>
      <SettingsSwitchRow
        label="Account rotation"
        help={{
          title: "Account rotation",
          body: "Codex stays on one anchor login. The local proxy serves each request from the selected pool account, without rewriting auth.json or restarting Codex.",
        }}
        checked={draft.enabled}
        onCheckedChange={(enabled) => {
          if (enabled && !draft.anchorProviderId) {
            toast.error("Select an anchor card first");
            return;
          }
          const next = { ...draft, enabled };
          setDraft(next);
          void onChange({ codexAccountRotation: next });
        }}
      />

      {draft.enabled && (
        <>
          <SettingsRow
            label="Anchor card"
            description="The official Codex card whose login authenticates the client. It must be the active Codex card."
          >
            <select
              className="w-full rounded-md border border-border bg-surface px-3 py-2 text-body text-fg-1"
              value={draft.anchorProviderId}
              onChange={(event) => void selectAnchor(event.target.value)}
            >
              <option value="">Select an official Codex card…</option>
              {anchorOptions.map((provider) => (
                <option key={provider.id} value={provider.id}>
                  {provider.name}
                </option>
              ))}
            </select>
          </SettingsRow>

          <SettingsRow label="Status">
            <div className="grid gap-2 text-caption text-fg-2 sm:grid-cols-2">
              <div>
                <div className="text-fg-1">Anchor login</div>
                <div>{status?.anchorLogin ?? "—"}</div>
              </div>
              <div>
                <div className="text-fg-1">Serving account</div>
                <div>{status?.selectedLogin ?? "—"}</div>
              </div>
              <div>
                <div className="text-fg-1">State</div>
                <div>
                  {status?.manualPaused
                    ? "manually paused"
                    : status?.autoPaused
                      ? "paused: all accounts at their floor"
                      : status?.running
                        ? "running"
                        : "stopped"}
                </div>
              </div>
              <div>
                <div className="text-fg-1">Next reset</div>
                <div>{formatTime(status?.nextReset)}</div>
              </div>
              <div>
                <div className="text-fg-1">Last switch</div>
                <div>{formatTime(status?.lastSwitchAt)}</div>
              </div>
              <div>
                <div className="text-fg-1">Last error</div>
                <div>{status?.lastError ?? "—"}</div>
              </div>
            </div>
            <div className="mt-3 flex flex-wrap gap-2">
              <Button variant="outline" size="compact" onClick={() => void runNow()}>
                Run now
              </Button>
              <Button variant="outline" size="compact" onClick={() => void togglePause()}>
                {status?.manualPaused ? "Resume" : "Pause"}
              </Button>
              {status?.accounts.length ? (
                <select
                  className="rounded-md border border-border bg-surface px-3 py-1.5 text-caption text-fg-1"
                  value={status.selectedAccountId ?? ""}
                  onChange={(event) => void selectServing(event.target.value)}
                >
                  <option value="">Auto (policy)</option>
                  {status.accounts.map((account) => (
                    <option key={account.accountId} value={account.accountId}>
                      Serve {account.login}
                    </option>
                  ))}
                </select>
              ) : null}
            </div>
          </SettingsRow>

          <SettingsRow
            label="Accounts"
            description="Lower priority numbers win. Remaining floors are percentages you keep untouched."
          >
            <div className="space-y-3">
              {draft.accounts.length === 0 ? (
                <p className="text-caption text-fg-2">
                  No accounts in the pool yet.
                </p>
              ) : (
                draft.accounts.map((account, index) => {
                  const live = statusById.get(account.accountId);
                  return (
                    <div
                      key={account.accountId}
                      className="rounded-md border border-border p-3"
                    >
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="text-body font-medium text-fg-1">
                          {account.login}
                        </span>
                        <span className="text-caption text-fg-2">
                          priority {account.priority}
                        </span>
                        {live?.reauthRequired ? (
                          <span className="text-caption text-red-500">
                            reauth required
                          </span>
                        ) : null}
                        <div className="ms-auto flex gap-1">
                          <Button
                            variant="outline"
                            size="compact"
                            disabled={index === 0}
                            onClick={() => reorder(index, -1)}
                          >
                            ↑
                          </Button>
                          <Button
                            variant="outline"
                            size="compact"
                            disabled={index === draft.accounts.length - 1}
                            onClick={() => reorder(index, 1)}
                          >
                            ↓
                          </Button>
                          <Button
                            variant="outline"
                            size="compact"
                            onClick={() =>
                              setDraft((current) => ({
                                ...current,
                                accounts: current.accounts
                                  .filter((_, position) => position !== index)
                                  .map((item, position) => ({
                                    ...item,
                                    priority: position + 1,
                                  })),
                              }))
                            }
                          >
                            Remove
                          </Button>
                        </div>
                      </div>
                      <div className="mt-2 grid gap-2 text-caption text-fg-2 sm:grid-cols-2">
                        <div>
                          <div>5h: {remaining(live?.fiveHourUsed, account.fiveHourMinRemaining)}</div>
                          <Input
                            type="number"
                            min={0}
                            max={100}
                            value={account.fiveHourMinRemaining}
                            onChange={(event) =>
                              updateAccount(index, {
                                fiveHourMinRemaining: clampPercent(event.target.value),
                              })
                            }
                          />
                        </div>
                        <div>
                          <div>
                            Weekly: {remaining(live?.weeklyUsed, account.weeklyMinRemaining)}
                          </div>
                          <Input
                            type="number"
                            min={0}
                            max={100}
                            value={account.weeklyMinRemaining}
                            onChange={(event) =>
                              updateAccount(index, {
                                weeklyMinRemaining: clampPercent(event.target.value),
                              })
                            }
                          />
                        </div>
                      </div>
                      <div className="mt-2 flex items-center gap-2 text-caption text-fg-2">
                        <Switch
                          checked={account.heartbeatEligible}
                          onCheckedChange={(checked) =>
                            updateAccount(index, { heartbeatEligible: checked })
                          }
                          aria-label="Heartbeat eligible"
                        />
                        <span>Allow idle heartbeat on this account</span>
                      </div>
                      <div className="mt-1 text-caption text-fg-2">
                        5h reset {formatTime(live?.fiveHourReset)} · weekly reset{" "}
                        {formatTime(live?.weeklyReset)}
                      </div>
                    </div>
                  );
                })
              )}
              {addable.length > 0 ? (
                <select
                  className="w-full rounded-md border border-border bg-surface px-3 py-2 text-body text-fg-1"
                  value=""
                  onChange={(event) => {
                    if (event.target.value) addAccount(event.target.value);
                  }}
                >
                  <option value="">Add a managed account…</option>
                  {addable.map((account) => (
                    <option key={account.id} value={account.id}>
                      {account.login}
                    </option>
                  ))}
                </select>
              ) : null}
            </div>
          </SettingsRow>

          <SettingsSwitchRow
            label="Idle heartbeat"
            help={{
              title: "Idle heartbeat",
              body: "After an idle period, send one tiny request through the serving account to start its 5-hour window early. Only accounts marked heartbeat-eligible are used, and it stops when you work.",
            }}
            checked={draft.heartbeat.enabled}
            onCheckedChange={(enabled) =>
              update({ heartbeat: { ...draft.heartbeat, enabled } })
            }
          />
          {draft.heartbeat.enabled ? (
            <SettingsRow label="Heartbeat timing">
              <div className="grid gap-3 sm:grid-cols-3">
                <label className="space-y-1 text-caption text-fg-2">
                  <span>Idle minutes</span>
                  <Input
                    type="number"
                    min={1}
                    value={draft.heartbeat.idleMinutes}
                    onChange={(event) =>
                      update({
                        heartbeat: {
                          ...draft.heartbeat,
                          idleMinutes: Math.max(1, Number(event.target.value) || 1),
                        },
                      })
                    }
                  />
                </label>
                <label className="space-y-1 text-caption text-fg-2">
                  <span>Repeat minutes</span>
                  <Input
                    type="number"
                    min={1}
                    value={draft.heartbeat.repeatMinutes}
                    onChange={(event) =>
                      update({
                        heartbeat: {
                          ...draft.heartbeat,
                          repeatMinutes: Math.max(1, Number(event.target.value) || 1),
                        },
                      })
                    }
                  />
                </label>
                <label className="space-y-1 text-caption text-fg-2">
                  <span>Target</span>
                  <select
                    className="w-full rounded-md border border-border bg-surface px-3 py-2 text-body text-fg-1"
                    value={draft.heartbeat.target}
                    onChange={(event) =>
                      update({
                        heartbeat: {
                          ...draft.heartbeat,
                          target: event.target.value,
                        },
                      })
                    }
                  >
                    <option value="preferred">Preferred eligible account</option>
                    {draft.accounts.map((account) => (
                      <option key={account.accountId} value={account.accountId}>
                        {account.login}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
            </SettingsRow>
          ) : null}

          <SettingsRow label="Advanced">
            <div className="grid gap-3 sm:grid-cols-3">
              <label className="space-y-1 text-caption text-fg-2">
                <span>Poll seconds</span>
                <Input
                  type="number"
                  min={5}
                  value={draft.pollSeconds}
                  onChange={(event) =>
                    update({
                      pollSeconds: Math.max(5, Number(event.target.value) || 5),
                    })
                  }
                />
              </label>
              <label className="space-y-1 text-caption text-fg-2">
                <span>Near-cap margin (%)</span>
                <Input
                  type="number"
                  min={0}
                  max={100}
                  value={draft.nearCapMargin}
                  onChange={(event) =>
                    update({
                      nearCapMargin: clampPercent(event.target.value),
                    })
                  }
                />
              </label>
              <label className="space-y-1 text-caption text-fg-2">
                <span>Manual override (seconds)</span>
                <Input
                  type="number"
                  min={0}
                  value={draft.manualOverrideSeconds}
                  onChange={(event) =>
                    update({
                      manualOverrideSeconds: Math.max(
                        0,
                        Number(event.target.value) || 0,
                      ),
                    })
                  }
                />
              </label>
            </div>
          </SettingsRow>

          <div className="flex justify-end px-4 py-3">
            <Button
              variant="solid"
              size="compact"
              disabled={saving}
              onClick={() => void save()}
            >
              Save rotation settings
            </Button>
          </div>
        </>
      )}
    </>
  );
}
