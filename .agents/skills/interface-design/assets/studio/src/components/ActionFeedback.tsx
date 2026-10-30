import { useCallback, useId, type ReactNode } from "react";
import { Toast } from "@base-ui/react/toast";
import { Info, X } from "lucide-react";
import { Button } from "./ui/button";
import "./action-feedback.css";

export function ActionFeedbackProvider({ children }: { children: ReactNode }) {
  return <Toast.Provider timeout={5000} limit={1}>{children}<FeedbackViewport /></Toast.Provider>;
}

function FeedbackViewport() {
  const { toasts } = Toast.useToastManager();
  return <Toast.Portal><Toast.Viewport className="action-feedback-viewport">
    {toasts.map((toast) => <Toast.Root key={toast.id} toast={toast} className="action-feedback">
      <Info size={18} aria-hidden="true" />
      <Toast.Content className="action-feedback-content"><Toast.Description>{toast.description}</Toast.Description></Toast.Content>
      <Toast.Close aria-label="Dismiss notification" render={<Button variant="ghost" size="icon-sm" />}><X size={16} /></Toast.Close>
    </Toast.Root>)}
  </Toast.Viewport></Toast.Portal>;
}

export function useActionFeedback() {
  const id = useId();
  const { add, close } = Toast.useToastManager();
  return useCallback((message: string) => {
    if (message) add({ id, description: message, priority: "high" });
    else close(id);
  }, [id, add, close]);
}
