import { describe, expect, it } from "vitest";
import type { OAuthQuotaState } from "../../../generated/bindings";
import { describeOAuthQuotaProtection } from "../OAuthQuotaProtectionStatus";

function state(overrides: Partial<OAuthQuotaState> = {}): OAuthQuotaState {
  return {
    provider_id: 1,
    protection_enabled: true,
    short_stop_percent: 10,
    long_stop_percent: 5,
    state: "threshold_reached",
    checked_at: 1000,
    reset_at: null,
    last_error: null,
    limits: {
      short_remaining_percent: 8,
      long_remaining_percent: 80,
      limit_short_label: "5h",
      limit_5h_text: "8%",
      limit_weekly_text: "80%",
      limit_5h_reset_at: null,
      limit_weekly_reset_at: null,
      reset_credit_available_count: null,
    },
    ...overrides,
  };
}

describe("OAuth quota protection feedback", () => {
  it("shows the affected window and threshold", () => {
    expect(describeOAuthQuotaProtection(state())).toContain("额度保护暂停：5h剩余 8% ≤ 10%");
  });
  it("distinguishes unavailable quota from actual exhaustion", () => {
    expect(describeOAuthQuotaProtection(state({ state: "quota_unverified" }))).toBe(
      "额度待确认，暂时跳过"
    );
    expect(describeOAuthQuotaProtection(state({ state: "quota_exhausted" }))).toBe(
      "OAuth 额度已耗尽，等待重置"
    );
  });
});
