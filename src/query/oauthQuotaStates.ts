import { useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { providerOAuthQuotaStates } from "../services/providers/providers";
import { listenDesktopEvent } from "../services/desktop/event";
import { appEventNames } from "../constants/appEvents";

export const oauthQuotaStatesKey = ["oauth-quota-states"] as const;

export function useOAuthQuotaStates(enabled = true) {
  const client = useQueryClient();
  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenDesktopEvent<number>(appEventNames.providerOAuthQuota, () => {
      void client.invalidateQueries({ queryKey: oauthQuotaStatesKey }, { cancelRefetch: false });
    })
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [client, enabled]);
  return useQuery({
    queryKey: oauthQuotaStatesKey,
    queryFn: providerOAuthQuotaStates,
    enabled,
    staleTime: 5000,
    refetchInterval: 10000,
    retry: false,
  });
}
