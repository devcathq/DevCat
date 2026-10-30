// "Link this chat to DevCat?": shown in every window when a chat sent a valid link code. The
// first answer wins; the request expires after 5 minutes.
import { MessageCircle } from "lucide-react";
import { useEffect } from "react";
import { toast } from "sonner";
import { Button, Dialog } from "../../components/ui";
import { errMsg } from "../../lib/api";
import { integrationsApi } from "./api";
import { setIntegrations, startIntegrations, useIntegrations } from "./store";

export function LinkPrompt() {
  useEffect(() => startIntegrations(), []);
  const req = useIntegrations((s) => s.status?.requests[0] ?? null);
  if (!req) return null;
  const answer = async (allow: boolean) => {
    try {
      setIntegrations(await integrationsApi.linkAnswer(req.requestId, allow));
      if (allow) toast.success(`${req.chatName} is linked`);
    } catch (e) {
      toast.error(errMsg(e));
    }
  };
  const kind = req.kind === "telegram" ? "Telegram" : "Discord";
  const can = [req.perms.notify && "get notifications", req.perms.approve && "allow or deny what agents ask", req.perms.requests && "send requests", req.perms.files && "get files", req.perms.connections && "use your DevCat connections"].filter(Boolean);
  return (
    <Dialog
      open
      onClose={() => void answer(false)}
      title={`Link this ${kind} chat to DevCat?`}
      width={480}
      footer={<><Button variant="ghost" onClick={() => void answer(false)}>Deny</Button><Button variant="primary" onClick={() => void answer(true)}>Link</Button></>}
    >
      <div className="space-y-3 text-[13px]">
        <div className="flex items-center gap-3 rounded-md border border-line bg-raised px-3 py-2.5">
          <MessageCircle className="size-5 text-muted" />
          <div className="min-w-0">
            <div className="font-semibold">{req.chatName}</div>
            <div className="truncate text-[11px] text-muted">{req.user.name} · via {req.botName} · {req.private ? "direct chat" : "group / channel"}</div>
          </div>
        </div>
        <p className="text-[12px] text-muted">
          {req.user.name} sent the link code. They'll be able to {can.join(", ")} for <span className="font-mono text-fg">{req.folder}</span>. Others in the chat are ignored.
        </p>
        <p className="text-[11px] text-faint">Didn't send a code just now? Click Deny. (Settings → Agent bots)</p>
      </div>
    </Dialog>
  );
}
