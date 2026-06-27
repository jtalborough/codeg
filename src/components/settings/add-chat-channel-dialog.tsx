"use client"

import { useCallback, useState } from "react"
import { Loader2 } from "lucide-react"
import { useTranslations } from "next-intl"

import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Switch } from "@/components/ui/switch"
import { createChatChannel, saveChatChannelToken } from "@/lib/api"
import type { ChannelType } from "@/lib/types"
import { toErrorMessage } from "@/lib/app-error"

interface AddChatChannelDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  onChannelAdded: () => void
}

export function AddChatChannelDialog({
  open,
  onOpenChange,
  onChannelAdded,
}: AddChatChannelDialogProps) {
  const t = useTranslations("ChatChannelSettings")
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [name, setName] = useState("")
  const [channelType, setChannelType] = useState<ChannelType>("telegram")
  const [token, setToken] = useState("")
  const [chatId, setChatId] = useState("")
  const [appId, setAppId] = useState("")
  const [baseUrl, setBaseUrl] = useState("https://ilinkai.weixin.qq.com")
  const [allowedSenders, setAllowedSenders] = useState("")
  const [workingDir, setWorkingDir] = useState("")
  const [agentType, setAgentType] = useState("")
  const [autoApprove, setAutoApprove] = useState(false)
  const [dailyReportEnabled, setDailyReportEnabled] = useState(false)
  const [dailyReportTime, setDailyReportTime] = useState("18:00")

  const resetForm = useCallback(() => {
    setName("")
    setChannelType("telegram")
    setToken("")
    setChatId("")
    setAppId("")
    setBaseUrl("https://ilinkai.weixin.qq.com")
    setAllowedSenders("")
    setWorkingDir("")
    setAgentType("")
    setAutoApprove(false)
    setDailyReportEnabled(false)
    setDailyReportTime("18:00")
    setError(null)
  }, [])

  const handleOpenChange = useCallback(
    (nextOpen: boolean) => {
      if (!nextOpen) resetForm()
      onOpenChange(nextOpen)
    },
    [onOpenChange, resetForm]
  )

  const handleSubmit = useCallback(async () => {
    if (!name.trim()) {
      setError(t("nameRequired"))
      return
    }
    if (channelType !== "weixin" && !token.trim()) {
      setError(t("tokenRequired"))
      return
    }
    if (channelType !== "weixin" && !chatId.trim()) {
      setError(t("chatIdRequired"))
      return
    }

    setLoading(true)
    setError(null)
    try {
      const allowedSendersArr = allowedSenders
        .split("\n")
        .map((s: string) => s.trim())
        .filter(Boolean)

      const baseConfig =
        channelType === "weixin"
          ? { base_url: baseUrl }
          : channelType === "lark"
            ? { app_id: appId, chat_id: chatId }
            : { chat_id: chatId }

      const configObj: Record<string, unknown> = {
        ...baseConfig,
        allowed_senders: allowedSendersArr,
      }
      if (workingDir.trim()) configObj.working_dir = workingDir.trim()
      if (agentType.trim()) configObj.agent_type = agentType.trim()
      if (autoApprove) configObj.auto_approve = true
      const configJson = JSON.stringify(configObj)

      const channel = await createChatChannel({
        name: name.trim(),
        channelType,
        configJson,
        enabled: true,
        dailyReportEnabled,
        dailyReportTime: dailyReportEnabled ? dailyReportTime : null,
      })

      if (channelType !== "weixin" && token.trim()) {
        await saveChatChannelToken(channel.id, token.trim())
      }

      handleOpenChange(false)
      onChannelAdded()
    } catch (err) {
      const msg = toErrorMessage(err)
      setError(msg)
    } finally {
      setLoading(false)
    }
  }, [
    name,
    token,
    chatId,
    channelType,
    appId,
    baseUrl,
    allowedSenders,
    workingDir,
    agentType,
    autoApprove,
    dailyReportEnabled,
    dailyReportTime,
    handleOpenChange,
    onChannelAdded,
    t,
  ])

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t("addChannel")}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          <div className="space-y-1.5">
            <label className="text-xs font-medium">{t("channelName")}</label>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t("channelNamePlaceholder")}
            />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium">{t("channelType")}</label>
            <Select
              value={channelType}
              onValueChange={(v) => setChannelType(v as ChannelType)}
            >
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="telegram">Telegram</SelectItem>
                <SelectItem value="lark">{t("lark")}</SelectItem>
                <SelectItem value="weixin">{t("weixin")}</SelectItem>
              </SelectContent>
            </Select>
          </div>

          {channelType === "lark" && (
            <div className="space-y-1.5">
              <label className="text-xs font-medium">App ID</label>
              <Input
                value={appId}
                onChange={(e) => setAppId(e.target.value)}
                placeholder="cli_xxxxx"
              />
            </div>
          )}

          {channelType !== "weixin" && (
            <div className="space-y-1.5">
              <label className="text-xs font-medium">
                {channelType === "telegram" ? "Bot Token" : "App Secret"}
              </label>
              <Input
                type="password"
                value={token}
                onChange={(e) => setToken(e.target.value)}
                placeholder={
                  channelType === "telegram" ? "123456:ABC-DEF..." : "xxxxx"
                }
              />
            </div>
          )}

          {channelType !== "weixin" && (
            <div className="space-y-1.5">
              <label className="text-xs font-medium">Chat ID</label>
              <Input
                value={chatId}
                onChange={(e) => setChatId(e.target.value)}
                placeholder={
                  channelType === "telegram" ? "-100123456789" : "oc_xxxxx"
                }
              />
            </div>
          )}

          {channelType === "weixin" && (
            <p className="text-xs text-muted-foreground">
              {t("weixinScanDescription")}
            </p>
          )}

          <div className="space-y-1.5">
            <label className="text-xs font-medium">Default folder</label>
            <Input
              value={workingDir}
              onChange={(e) => setWorkingDir(e.target.value)}
              placeholder="/home/rai/src/codeg"
              className="font-mono text-xs"
            />
            <p className="text-[11px] text-muted-foreground">
              The folder this channel&apos;s agent works in by default. Plain
              messages talk to that agent; override with /folder.
            </p>
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium">Default agent</label>
            <Input
              value={agentType}
              onChange={(e) => setAgentType(e.target.value)}
              placeholder="claude_code"
              className="font-mono text-xs"
            />
            <p className="text-[11px] text-muted-foreground">
              Agent type id (e.g. claude_code, codex, gemini, openclaw).
              Override with /agent.
            </p>
          </div>

          <div className="flex items-center justify-between gap-3">
            <div className="space-y-0.5">
              <label className="text-xs font-medium">
                Auto-approve tool calls
              </label>
              <p className="text-[11px] text-muted-foreground">
                Skip per-action permission prompts for this channel. Use for a
                trusted persona working in its own workspace.
              </p>
            </div>
            <Switch checked={autoApprove} onCheckedChange={setAutoApprove} />
          </div>

          <div className="space-y-1.5">
            <label className="text-xs font-medium">Allowed Sender IDs</label>
            <Textarea
              value={allowedSenders}
              onChange={(e) => setAllowedSenders(e.target.value)}
              placeholder={"123456789\n987654321"}
              rows={3}
              className="font-mono text-xs"
            />
            <p className="text-[11px] text-muted-foreground">
              One sender ID per line. Only these senders may drive agents via
              this channel. Leave empty to block everyone (fail-closed). A
              blocked sender is told their own ID so you can add it here.
            </p>
          </div>

          <div className="flex items-center justify-between">
            <label className="text-xs font-medium">{t("dailyReport")}</label>
            <Switch
              checked={dailyReportEnabled}
              onCheckedChange={setDailyReportEnabled}
            />
          </div>

          {dailyReportEnabled && (
            <div className="space-y-1.5">
              <label className="text-xs font-medium">
                {t("dailyReportTime")}
              </label>
              <Input
                type="time"
                value={dailyReportTime}
                onChange={(e) => setDailyReportTime(e.target.value)}
              />
            </div>
          )}

          {error && (
            <div className="rounded-md border border-red-500/30 bg-red-500/5 px-3 py-2 text-xs text-red-400">
              {error}
            </div>
          )}
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => handleOpenChange(false)}
            disabled={loading}
          >
            {t("cancel")}
          </Button>
          <Button onClick={handleSubmit} disabled={loading}>
            {loading && <Loader2 className="h-3.5 w-3.5 animate-spin mr-1" />}
            {t("create")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
