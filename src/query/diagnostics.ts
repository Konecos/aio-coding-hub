import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useDocumentVisibility } from "../hooks/useDocumentVisibility";
import {
  diagnosticsClear,
  diagnosticsConfigure,
  diagnosticsEvents,
  diagnosticsSnapshot,
} from "../services/gateway/diagnostics";

const root = ["gateway", "diagnostics"] as const;

export function useDiagnosticsSnapshot(live = false) {
  const visible = useDocumentVisibility();
  return useQuery({
    queryKey: [...root, "snapshot"],
    queryFn: diagnosticsSnapshot,
    refetchInterval: live && visible ? 1000 : false,
    retry: false,
  });
}

export function useDiagnosticsEvents(traceId: string | null, live: boolean) {
  const visible = useDocumentVisibility();
  return useQuery({
    queryKey: [...root, "events", traceId],
    queryFn: () => diagnosticsEvents(traceId!),
    enabled: traceId != null,
    refetchInterval: live && visible ? 1000 : false,
    retry: false,
  });
}

export function useDiagnosticsConfigure() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({
      enabled,
      days,
      storageLimitBytes,
    }: {
      enabled: boolean;
      days: number;
      storageLimitBytes: number;
    }) => diagnosticsConfigure(enabled, days, storageLimitBytes),
    onSuccess: async () => {
      await client.cancelQueries({ queryKey: root });
      client.setQueriesData({ queryKey: [...root, "events"] }, []);
      return client.invalidateQueries({ queryKey: root });
    },
  });
}

export function useDiagnosticsClear() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: diagnosticsClear,
    onSuccess: async () => {
      await client.cancelQueries({ queryKey: root });
      client.setQueriesData({ queryKey: [...root, "events"] }, []);
      return client.invalidateQueries({ queryKey: root });
    },
  });
}
