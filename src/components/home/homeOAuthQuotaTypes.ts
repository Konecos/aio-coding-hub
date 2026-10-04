import {
  hasInsufficientOAuthQuota,
  type CliKey,
  type OAuthLimitsResult,
} from "../../services/providers/providers";
import type { OAuthQuotaState } from "../../generated/bindings";

export type HomeOAuthQuotaRowState = "idle" | "loading" | "success" | "error";

export type HomeOAuthQuotaRow = {
  providerId: number;
  cliKey: CliKey;
  providerName: string;
  enabled: boolean;
  state: HomeOAuthQuotaRowState;
  limits: OAuthLimitsResult | null;
  quotaState?: OAuthQuotaState | null;
  error: string | null;
  resetting?: boolean;
  resetError?: string | null;
};

export function hasHomeOAuthQuotaText(limits: OAuthLimitsResult | null): boolean {
  return Boolean(limits?.limit_5h_text || limits?.limit_weekly_text);
}

export function hasInsufficientHomeOAuthQuota(limits: OAuthLimitsResult | null): boolean {
  return hasInsufficientOAuthQuota(limits);
}
