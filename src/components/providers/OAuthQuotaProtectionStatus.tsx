import type { OAuthQuotaState } from "../../generated/bindings";
import { formatUnixSeconds } from "../../utils/formatters";

export function describeOAuthQuotaProtection(state: OAuthQuotaState): string {
  if (state.state === "quota_unverified") return "额度待确认，暂时跳过";
  if (state.state === "quota_exhausted") return "OAuth 额度已耗尽，等待重置";
  const windows = [
    [
      state.limits?.limit_short_label ?? "短窗",
      state.limits?.short_remaining_percent,
      state.short_stop_percent,
    ],
    [
      state.limits?.limit_short_label === "5h" ? "每周" : "长窗",
      state.limits?.long_remaining_percent,
      state.long_stop_percent,
    ],
  ] as const;
  const descriptions = windows.flatMap(([label, remaining, threshold]) => {
    if (threshold == null) return [];
    const value = remaining == null ? "未知" : `${Number(remaining.toFixed(2))}%`;
    return [
      `${label}剩余 ${value}${remaining != null && remaining <= threshold ? " ≤ " : "，停止阈值 "}${threshold}%`,
    ];
  });
  return `${state.state === "threshold_reached" ? "额度保护暂停：" : "额度保护："}${descriptions.join("；")}`;
}

export function OAuthQuotaProtectionStatus(props: {
  state?: OAuthQuotaState | null;
  protectionEnabled?: boolean;
}) {
  const state = props.state;
  if (!state?.protection_enabled) {
    return props.protectionEnabled ? (
      <span className="text-xs text-amber-700 dark:text-amber-400">额度保护状态待确认</span>
    ) : null;
  }
  const paused = state.state !== "available";
  const detail = [
    describeOAuthQuotaProtection(state),
    state.checked_at != null
      ? `最后成功更新：${formatUnixSeconds(state.checked_at)}`
      : "尚未成功获取额度",
    state.reset_at != null
      ? `预计重置：${formatUnixSeconds(state.reset_at)}，恢复以刷新结果为准`
      : null,
    state.last_error,
  ]
    .filter(Boolean)
    .join("\n");
  return (
    <span
      title={detail}
      className={
        paused ? "text-xs text-amber-700 dark:text-amber-400" : "text-xs text-muted-foreground"
      }
    >
      {describeOAuthQuotaProtection(state)}
    </span>
  );
}
