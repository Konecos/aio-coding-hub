import { FormField } from "../../ui/FormField";
import { Input } from "../../ui/Input";
import { Button } from "../../ui/Button";
import { formatUnixSeconds } from "../../utils/formatters";
import type { UseProviderEditorFormReturn } from "./useProviderEditorForm";
import { useOAuthQuotaStates } from "../../query/oauthQuotaStates";

export function OAuthSection(props: { form: UseProviderEditorFormReturn }) {
  const {
    register,
    watch,
    setValue,
    editingProviderId,
    saving,
    cliKey,
    oauthStatus,
    oauthLoading,
    oauthDeviceFlow,
    oauthDevicePolling,
    oauthDeviceError,
    handleOAuthLogin,
    handleOAuthDeviceLogin,
    handleOAuthRefresh,
    handleOAuthDisconnect,
  } = props.form;
  const quotas = useOAuthQuotaStates(editingProviderId != null);
  const quota = quotas.data?.find((row) => row.provider_id === editingProviderId);

  return (
    <>
      <FormField label="名称">
        <Input placeholder="default" {...register("name")} />
      </FormField>

      <FormField label="OAuth 连接">
        <div className="rounded-md border border-border bg-secondary p-3 dark:border-border dark:bg-secondary/50">
          {oauthLoading && !oauthDeviceFlow ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <span className="animate-spin">⏳</span>
              <span>处理中...</span>
            </div>
          ) : oauthStatus?.connected ? (
            <div className="space-y-2">
              {oauthStatus.email && (
                <p className="text-sm text-secondary-foreground">
                  <span className="font-medium">账号：</span>
                  {oauthStatus.email}
                </p>
              )}
              {oauthStatus.expires_at && (
                <p className="text-xs text-muted-foreground">
                  <span className="font-medium">到期：</span>
                  {formatUnixSeconds(oauthStatus.expires_at)}
                </p>
              )}
              <div className="flex items-center gap-2">
                <Button
                  onClick={handleOAuthRefresh}
                  variant="secondary"
                  disabled={saving || oauthLoading}
                >
                  刷新 Token
                </Button>
                <Button
                  onClick={handleOAuthDisconnect}
                  variant="secondary"
                  disabled={saving || oauthLoading}
                >
                  断开连接
                </Button>
              </div>
            </div>
          ) : (
            <div className="space-y-3">
              <p className="text-sm text-muted-foreground">未连接 OAuth</p>
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  onClick={handleOAuthLogin}
                  variant="primary"
                  disabled={saving || oauthLoading}
                >
                  OAuth 登录
                </Button>
                {cliKey === "codex" || cliKey === "grok" ? (
                  <Button
                    onClick={handleOAuthDeviceLogin}
                    variant="secondary"
                    disabled={saving || (oauthLoading && !oauthDevicePolling)}
                  >
                    设备码登录
                  </Button>
                ) : null}
              </div>
              {cliKey === "codex" || cliKey === "grok" ? (
                <p className="text-xs leading-relaxed text-muted-foreground">
                  若当前环境下 localhost
                  回调不稳定，可改用设备码登录，在浏览器中输入验证码完成授权。
                </p>
              ) : null}
              {oauthDeviceFlow ? (
                <div className="rounded-md border border-border bg-card p-3 text-sm text-card-foreground">
                  <p>
                    <span className="font-medium">验证码：</span>
                    <code className="ml-2 rounded bg-muted px-2 py-1 font-mono">
                      {oauthDeviceFlow.user_code}
                    </code>
                  </p>
                  <p className="mt-2 text-xs leading-relaxed text-muted-foreground">
                    请在浏览器中打开 {oauthDeviceFlow.verification_uri}
                    ，输入上面的验证码后返回本窗口等待完成。
                  </p>
                  {oauthDevicePolling ? (
                    <p className="mt-2 text-xs text-muted-foreground">等待授权中...</p>
                  ) : null}
                  {oauthDeviceError ? (
                    <p className="mt-2 text-xs text-destructive">{oauthDeviceError}</p>
                  ) : null}
                </div>
              ) : null}
            </div>
          )}
        </div>
      </FormField>

      <FormField label="剩余额度保护">
        <div className="space-y-3 rounded-md border border-border p-3">
          {(
            [
              [
                "oauth_short_window_stop_percent",
                cliKey === "codex" || cliKey === "claude" ? "5 小时" : "短窗",
                "short_remaining_percent",
                "10",
              ],
              [
                "oauth_long_window_stop_percent",
                cliKey === "codex" || cliKey === "claude" ? "每周" : "长窗",
                "long_remaining_percent",
                "5",
              ],
            ] as const
          ).map(([field, label, percent, initial]) => {
            const value = watch(field) ?? "";
            const active = value !== "";
            const unsupported =
              cliKey === "grok" ||
              (quota?.checked_at != null && !quota.last_error && quota.limits?.[percent] == null);
            return (
              <div key={field} className="space-y-1">
                <div className="flex flex-wrap items-center gap-2 text-sm">
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={active}
                      disabled={saving || (!active && unsupported)}
                      onChange={(event) =>
                        setValue(field, event.target.checked ? initial : "", { shouldDirty: true })
                      }
                    />
                    {label}剩余 ≤
                  </label>
                  <Input
                    aria-label={`${label}停止阈值`}
                    type="number"
                    min="0"
                    max="99"
                    step="1"
                    className="w-20"
                    disabled={saving || !active}
                    {...register(field)}
                  />
                  <span>% 时暂停使用</span>
                </div>
                {unsupported ? (
                  <p className="text-xs text-muted-foreground">该窗口暂不支持百分比额度保护。</p>
                ) : null}
              </div>
            );
          })}
          <p className="text-xs leading-relaxed text-muted-foreground">
            任一窗口达到阈值后自动切换供应商，刷新确认额度恢复后自动恢复。开启保护后，额度未知或过期时暂时跳过。基于最近额度数据判断，存在刷新延迟。
          </p>
          {cliKey === "gemini" ? (
            <p className="text-xs leading-relaxed text-muted-foreground">
              Gemini 按窗口内各模型的最低剩余百分比判断，命中后暂停整个供应商。
            </p>
          ) : null}
        </div>
      </FormField>

      <FormField label="价格倍率">
        <Input
          type="number"
          min="0.0001"
          step="0.01"
          placeholder="1.0"
          {...register("cost_multiplier")}
        />
      </FormField>
    </>
  );
}
