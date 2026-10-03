import { describe, expect, it } from "vitest";
import {
  cloneProviderModelPolicy,
  normalizeProviderModelPolicyDraft,
  validateProviderModelPolicy,
} from "../providerModelPolicy";

describe("providerModelPolicy", () => {
  it("preserves supplier and mapping profiles through editing and duplication", () => {
    const policy = {
      version: 1,
      mode: "selected" as const,
      modelPatterns: [" deepseek-flash "],
      codexProfile: "deepseek" as const,
      mappings: [
        {
          source: " alias ",
          target: " deepseek-v4-pro ",
          codexProfile: "function_compatible" as const,
        },
      ],
    };
    const normalized = normalizeProviderModelPolicyDraft(cloneProviderModelPolicy(policy));
    expect(normalized.codexProfile).toBe("deepseek");
    expect(normalized.mappings).toEqual([
      { source: "alias", target: "deepseek-v4-pro", codexProfile: "function_compatible" },
    ]);
    expect(validateProviderModelPolicy(normalized)).toBeNull();
    expect(policy.mappings[0].source).toBe(" alias ");
  });
  it("accepts large policies and long Unicode model names", () => {
    expect(
      validateProviderModelPolicy({
        version: 1,
        mode: "selected",
        modelPatterns: ["模型".repeat(201), ...Array.from({ length: 500 }, (_, i) => `model-${i}`)],
        mappings: [],
      })
    ).toBeNull();
  });

  it("requires mapping targets and accepts a mapping as selected-model support", () => {
    expect(
      validateProviderModelPolicy({
        version: 1,
        mode: "selected",
        modelPatterns: [],
        mappings: [{ source: "gpt-5.6-luna", target: "deepseek-v4-flash" }],
      })
    ).toBeNull();
    expect(
      validateProviderModelPolicy({
        version: 1,
        mode: "all",
        modelPatterns: [],
        mappings: [{ source: "gpt-5.6-luna", target: "" }],
      })
    ).toBe("上游模型不能为空");
  });
});
