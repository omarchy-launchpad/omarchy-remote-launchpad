import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, existsSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { DynamicBorder, formatSize, keyHint, truncateTail } from "@earendil-works/pi-coding-agent";
import { StringEnum } from "@earendil-works/pi-ai";
import { Container, matchesKey, type SelectItem, SelectList, Text, truncateToWidth, visibleWidth } from "@earendil-works/pi-tui";
import { Type } from "typebox";

const TOOL_NAME = "olaunchpad_shell";
const WIDGET_NAME = "olaunchpad-shell";
const MAX_CAPTURE_BYTES = 2 * 1024 * 1024;
const MAX_CAPTURE_LINES = 10_000;

type Action =
	| "start"
	| "restart"
	| "stop"
	| "status"
	| "logs"
	| "errors"
	| "show"
	| "launch"
	| "troubleshoot";

type ProcessState = "stopped" | "starting" | "running" | "stopping" | "failed";

interface ToolDetails {
	action: Action;
	state: ProcessState;
	watcherPid?: number;
	appPid?: number;
	logPath: string;
	exitCode?: number | null;
}

const Parameters = Type.Object({
	action: StringEnum(
		["start", "restart", "stop", "status", "logs", "errors", "show", "launch", "troubleshoot"] as const,
		{ description: "Dev-process or launchpad operation" },
	),
	query: Type.Optional(
		Type.String({ description: "Application name or desktop ID to search for and launch" }),
	),
	lines: Type.Optional(
		Type.Integer({ minimum: 1, maximum: 500, description: "Number of log lines to return (default 120)" }),
	),
});

function tailLines(text: string, count: number): string {
	return text.split(/\r?\n/).slice(-count).join("\n").trimEnd();
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function actionLabel(action: Action): string {
	return {
		start: "Start watcher",
		restart: "Restart watcher",
		stop: "Stop watcher",
		status: "Check status",
		logs: "View live logs",
		errors: "View error history",
		show: "Show Launchpad",
		launch: "Launch application",
		troubleshoot: "Run diagnostics",
	}[action];
}

function actionGlyph(action: Action): string {
	if (action === "start" || action === "restart") return "↻";
	if (action === "stop") return "■";
	if (action === "show" || action === "launch") return "↗";
	if (action === "logs" || action === "errors" || action === "troubleshoot") return "≡";
	return "●";
}

export default function olaunchpadShell(pi: ExtensionAPI) {
	let cwd = "";
	let child: ChildProcessWithoutNullStreams | undefined;
	let state: ProcessState = "stopped";
	let appPid: number | undefined;
	let lastExitCode: number | null | undefined;
	let capture = "";
	let partialLine = "";
	let generation = 0;
	let sessionContext: ExtensionContext | undefined;
	let logPath = join(tmpdir(), "olaunchpad-shell.log");

	function liveAppPid(): number | undefined {
		if (!appPid) return undefined;
		try {
			process.kill(appPid, 0);
			return appPid;
		} catch {
			appPid = undefined;
			if (child && state === "running") state = "starting";
			return undefined;
		}
	}

	function details(action: Action): ToolDetails {
		return {
			action,
			state,
			watcherPid: child?.pid,
			appPid: liveAppPid(),
			logPath,
			exitCode: lastExitCode,
		};
	}

	function statusText(): string {
		const watcher = child?.pid ? `watcher ${child.pid}` : "no watcher";
		const currentAppPid = liveAppPid();
		const app = currentAppPid ? `app ${currentAppPid}` : "app pending";
		const exit = lastExitCode === undefined ? "" : `, last exit ${lastExitCode ?? "signal"}`;
		return `${state}: ${watcher}, ${app}${exit}`;
	}

	function updateUi(): void {
		const ctx = sessionContext;
		if (!ctx?.hasUI) return;
		const color = state === "running" ? "success" : state === "failed" ? "error" : "warning";
		const dot = state === "running" ? "●" : state === "failed" ? "×" : "◌";
		ctx.ui.setStatus(WIDGET_NAME, ctx.ui.theme.fg(color, `${dot} launchpad ${state}`));
		ctx.ui.setWidget(
			WIDGET_NAME,
			[
				ctx.ui.theme.fg(color, `${dot} `) +
					ctx.ui.theme.fg("accent", "Launchpad") +
					ctx.ui.theme.fg("dim", `  ${statusText()}  ·  /olaunchpad`),
			],
			{ placement: "belowEditor" },
		);
	}

	function record(chunk: Buffer | string): void {
		const text = partialLine + chunk.toString();
		const parts = text.split(/(?<=\n)/);
		partialLine = parts.at(-1)?.endsWith("\n") ? "" : (parts.pop() ?? "");
		const complete = parts.join("");
		if (!complete) return;

		capture += complete;
		const captureLines = capture.split("\n");
		if (captureLines.length > MAX_CAPTURE_LINES) {
			capture = captureLines.slice(-MAX_CAPTURE_LINES).join("\n");
		}
		if (Buffer.byteLength(capture) > MAX_CAPTURE_BYTES) {
			capture = capture.slice(-MAX_CAPTURE_BYTES);
		}
		try {
			appendFileSync(logPath, complete, { encoding: "utf8", mode: 0o600 });
		} catch {
			// In-memory logs remain available if the temporary log cannot be written.
		}

		for (const line of complete.split("\n")) {
			const match = line.match(/omarchy-launchpad dev process (\d+)/);
			if (match) {
				appPid = Number(match[1]);
				state = "running";
				updateUi();
			} else if (/omarchy-launchpad exited with status|omarchy-launchpad is already running/.test(line)) {
				appPid = undefined;
				if (child) state = "starting";
				updateUi();
			}
		}
	}

	async function waitForApp(timeoutMs = 30_000): Promise<void> {
		const started = Date.now();
		while (!liveAppPid() && child && Date.now() - started < timeoutMs) {
			await new Promise((resolve) => setTimeout(resolve, 100));
		}
		if (!child) throw new Error(`make dev is not running (${statusText()})`);
		if (!liveAppPid()) {
			throw new Error(
				`Timed out waiting for the omarchy-launchpad app (${statusText()}). ` +
					"Check for another make dev/omarchy-launchpad instance and inspect the logs.",
			);
		}
	}

	async function findExternalWatcher(): Promise<number | undefined> {
		const processes = await pi.exec("/bin/ps", ["-axo", "pid=,command="], { timeout: 3_000 });
		if (processes.code !== 0) return undefined;
		const candidates = processes.stdout
			.split("\n")
			.map((line) => line.match(/^\s*(\d+)\s+.*(?:\/|\b)make\s+dev(?:\s|$)/))
			.flatMap((match) => (match ? [Number(match[1])] : []))
			.filter((pid) => pid !== child?.pid);
		for (const pid of candidates) {
			const openFiles = await pi.exec("/usr/sbin/lsof", ["-a", "-p", String(pid), "-d", "cwd", "-Fn"], {
				timeout: 3_000,
			});
			if (openFiles.stdout.split("\n").includes(`n${cwd}`)) return pid;
		}
		return undefined;
	}

	async function start(): Promise<string> {
		if (child) return `Already running: ${statusText()}`;
		if (process.platform !== "darwin") throw new Error("olaunchpad-shell requires macOS");
		if (!existsSync(join(cwd, "Makefile"))) throw new Error(`No Makefile in ${cwd}`);
		const externalPid = await findExternalWatcher();
		if (externalPid) {
			throw new Error(
				`Another make dev watcher already owns this checkout (PID ${externalPid}). ` +
					"Stop it before asking olaunchpad-shell to start, so rebuilds do not race.",
			);
		}

		generation += 1;
		const thisGeneration = generation;
		state = "starting";
		appPid = undefined;
		lastExitCode = undefined;
		capture = "";
		partialLine = "";
		updateUi();

		const proc = spawn("make", ["dev"], {
			cwd,
			env: { ...process.env, NO_COLOR: "1" },
			detached: true,
			stdio: ["pipe", "pipe", "pipe"],
		});
		child = proc;
		proc.stdout.on("data", record);
		proc.stderr.on("data", record);
		proc.on("error", (error) => record(`olaunchpad-shell: ${error.message}\n`));
		proc.on("close", (code, signal) => {
			if (generation !== thisGeneration) return;
			record(`olaunchpad-shell: make dev exited (${code ?? signal ?? "unknown"})\n`);
			lastExitCode = code;
			child = undefined;
			appPid = undefined;
			state = state === "stopping" ? "stopped" : "failed";
			updateUi();
			if (state === "failed" && sessionContext?.hasUI) {
				sessionContext.ui.notify("olaunchpad make dev exited; inspect /olaunchpad logs", "error");
			}
		});

		return `Started make dev (watcher PID ${proc.pid ?? "pending"}). Log: ${logPath}`;
	}

	async function stop(): Promise<string> {
		const proc = child;
		if (!proc) {
			state = "stopped";
			appPid = undefined;
			updateUi();
			return "make dev is already stopped.";
		}

		state = "stopping";
		updateUi();
		const closed = new Promise<void>((resolve) => proc.once("close", () => resolve()));
		if (proc.pid) {
			try {
				process.kill(-proc.pid, "SIGTERM");
			} catch {
				proc.kill("SIGTERM");
			}
		}
		await Promise.race([closed, new Promise((resolve) => setTimeout(resolve, 4_000))]);
		if (child === proc && proc.pid) {
			try {
				process.kill(-proc.pid, "SIGKILL");
			} catch {
				proc.kill("SIGKILL");
			}
			await Promise.race([closed, new Promise((resolve) => setTimeout(resolve, 1_000))]);
		}
		return "Stopped make dev and its omarchy-launchpad child process.";
	}

	async function restart(): Promise<string> {
		await stop();
		const result = await start();
		return `Restarted. ${result}`;
	}

	async function show(signal?: AbortSignal): Promise<string> {
		if (!child) await start();
		await waitForApp();
		const binary = join(cwd, "target/debug/omarchy-launchpad");
		if (!existsSync(binary)) throw new Error(`Missing ${binary}; make dev has not completed a build`);
		const result = await pi.exec(binary, [], { cwd, signal, timeout: 5_000 });
		if (result.code !== 0) {
			throw new Error(`Could not show the launchpad: ${result.stderr || result.stdout || `exit ${result.code}`}`);
		}
		return "omarchy-launchpad is visible and its search field is focused.";
	}

	async function launch(query: string, signal?: AbortSignal): Promise<string> {
		const trimmed = query.trim();
		if (!trimmed) throw new Error("launch requires a non-empty query");
		await show(signal);
		const pid = liveAppPid();
		if (!pid) throw new Error("omarchy-launchpad exited before launchpad automation could run");
		const script = `on run argv
  set appQuery to item 1 of argv
  set appPid to (item 2 of argv) as integer
  tell application "System Events"
    set matches to every process whose unix id is appPid
    if (count of matches) is 0 then error "omarchy-launchpad process not found"
    tell item 1 of matches
      set frontmost to true
      delay 0.2
      keystroke "a" using command down
      keystroke appQuery
      delay 0.8
      key code 36
    end tell
  end tell
end run`;
		const result = await pi.exec("/usr/bin/osascript", ["-e", script, trimmed, String(pid)], {
			cwd,
			signal,
			timeout: 10_000,
		});
		if (result.code !== 0) {
			throw new Error(
				`Launchpad automation failed: ${result.stderr || result.stdout || `exit ${result.code}`}. ` +
					"Grant Accessibility permission to the terminal running pi, then retry.",
			);
		}
		return `Searched for ${JSON.stringify(trimmed)} and activated the first matching application.`;
	}

	function logs(lines = 120): string {
		const text = capture + partialLine;
		return tailLines(text, lines) || "No make dev output captured yet.";
	}

	function errors(lines = 120): string {
		const path = join(cwd, "errors.txt");
		if (!existsSync(path)) return `${path} does not exist yet.`;
		return tailLines(readFileSync(path, "utf8"), lines) || `${path} is empty.`;
	}

	function troubleshooting(lines = 160): string {
		const configPath = join(cwd, "omarchy-launchpad.local");
		const config = existsSync(configPath)
			? readFileSync(configPath, "utf8").trim()
				? "configured"
				: "empty"
			: "missing";
		return [
			`State: ${statusText()}`,
			`Project: ${cwd}`,
			`omarchy-launchpad.local: ${config}`,
			`Captured log: ${logPath}`,
			"",
			"=== current make dev output ===",
			logs(lines),
			"",
			"=== append-only errors.txt tail (may contain stale incidents) ===",
			errors(lines),
		].join("\n");
	}

	async function perform(action: Action, query?: string, lines = 120, signal?: AbortSignal): Promise<string> {
		switch (action) {
			case "start":
				return start();
			case "restart":
				return restart();
			case "stop":
				return stop();
			case "status":
				return `${statusText()}\nCaptured log: ${logPath}`;
			case "logs":
				return logs(lines);
			case "errors":
				return errors(lines);
			case "show":
				return show(signal);
			case "launch":
				return launch(query ?? "", signal);
			case "troubleshoot":
				return troubleshooting(lines);
		}
	}

	async function showOutputOverlay(ctx: ExtensionContext, title: string, output: string): Promise<void> {
		const sourceLines = output.split(/\r?\n/);
		const viewportLines = 18;
		let offset = Math.max(0, sourceLines.length - viewportLines);

		await ctx.ui.custom<void>(
			(tui, theme, _keybindings, done) => ({
				render(width: number) {
					const innerWidth = Math.max(1, width - 2);
					const maxOffset = Math.max(0, sourceLines.length - viewportLines);
					offset = Math.max(0, Math.min(maxOffset, offset));
					const visible = sourceLines.slice(offset, offset + viewportLines);
					const pad = (value: string) => {
						const clipped = truncateToWidth(value, innerWidth, "");
						return clipped + " ".repeat(Math.max(0, innerWidth - visibleWidth(clipped)));
					};
					const row = (value: string) =>
						theme.fg("border", "│") + pad(value) + theme.fg("border", "│");
					const position = sourceLines.length > viewportLines
						? `  ${offset + 1}–${Math.min(offset + viewportLines, sourceLines.length)} of ${sourceLines.length}`
						: "";
					return [
						theme.fg("borderAccent", `╭${"─".repeat(innerWidth)}╮`),
						row(` ${theme.fg("accent", theme.bold(title))}${theme.fg("dim", position)}`),
						theme.fg("borderMuted", `├${"─".repeat(innerWidth)}┤`),
						...visible.map((line) => row(` ${theme.fg("toolOutput", line)}`)),
						...Array.from({ length: Math.max(0, viewportLines - visible.length) }, () => row("")),
						theme.fg("borderMuted", `├${"─".repeat(innerWidth)}┤`),
						row(` ${theme.fg("dim", "↑↓ scroll  ·  ctrl+u/d page  ·  esc close")}`),
						theme.fg("borderAccent", `╰${"─".repeat(innerWidth)}╯`),
					];
				},
				handleInput(data: string) {
					if (matchesKey(data, "escape") || matchesKey(data, "ctrl+c")) {
						done();
						return;
					}
					if (matchesKey(data, "up")) offset -= 1;
					else if (matchesKey(data, "down")) offset += 1;
					else if (matchesKey(data, "ctrl+u")) offset -= viewportLines;
					else if (matchesKey(data, "ctrl+d")) offset += viewportLines;
					else if (matchesKey(data, "home")) offset = 0;
					else if (matchesKey(data, "end")) offset = sourceLines.length;
					else return;
					tui.requestRender();
				},
				invalidate() {},
			}),
			{
				overlay: true,
				overlayOptions: { width: "80%", minWidth: 56, maxHeight: "85%", anchor: "center", margin: 1 },
			},
		);
	}

	pi.registerTool({
		name: TOOL_NAME,
		label: "Olaunchpad Shell",
		description:
			"Control the repository's long-running make dev process, inspect current or historical logs, show omarchy-launchpad, and launch an application through its search UI. Log output is tail-truncated before being returned.",
		promptSnippet: "Start, restart, inspect, and interactively test the Omarchy Remote Launchpad dev app",
		promptGuidelines: [
			"Use olaunchpad_shell rather than starting a second make dev watcher; use its restart action after lifecycle changes.",
			"Use olaunchpad_shell troubleshooting output as evidence and remember that errors.txt is append-only and can contain stale failures.",
		],
		parameters: Parameters,
		async execute(_toolCallId, params, signal, onUpdate) {
			onUpdate?.({ content: [{ type: "text", text: `${params.action}…` }] });
			const output = await perform(params.action, params.query, params.lines ?? 120, signal);
			const truncated = truncateTail(output, { maxLines: 500, maxBytes: 50 * 1024 });
			let text = truncated.content;
			if (truncated.truncated) {
				text += `\n\n[Output truncated to ${truncated.outputLines} lines/${formatSize(truncated.outputBytes)}. Full captured dev output: ${logPath}]`;
			}
			return { content: [{ type: "text", text }], details: details(params.action) };
		},
		renderCall(args, theme) {
			const query = args.query ? `  ${JSON.stringify(args.query)}` : "";
			return new Text(
				theme.fg("toolTitle", theme.bold(`${actionGlyph(args.action)} Launchpad`)) +
					theme.fg("muted", `  ${actionLabel(args.action)}`) +
					theme.fg("dim", query),
				0,
				0,
			);
		},
		renderResult(result, { expanded, isPartial }, theme, context) {
			const action = context.args.action;
			if (isPartial) {
				return new Text(theme.fg("warning", `◌ ${actionLabel(action)}…`), 0, 0);
			}

			const info = result.details as ToolDetails | undefined;
			const content = result.content[0]?.type === "text" ? result.content[0].text : "";
			const failed = context.isError || info?.state === "failed";
			const color = failed ? "error" : "success";
			const mark = failed ? "×" : "✓";
			let summary: string;

			switch (action) {
				case "logs":
				case "errors":
				case "troubleshoot": {
					const count = content ? content.split(/\r?\n/).length : 0;
					summary = `${actionLabel(action)} · ${count} line${count === 1 ? "" : "s"}`;
					break;
				}
				case "launch":
					summary = context.args.query ? `Launched ${context.args.query}` : actionLabel(action);
					break;
				case "show":
					summary = "Launchpad focused";
					break;
				default:
					summary = actionLabel(action);
			}

			let text = theme.fg(color, `${mark} ${summary}`);
			if (info?.watcherPid) text += theme.fg("muted", `  ·  watcher ${info.watcherPid}`);
			if (info?.appPid) text += theme.fg("muted", `  ·  app ${info.appPid}`);
			if (!expanded && content.includes("\n")) {
				text += theme.fg("dim", `  (${keyHint("app.tools.expand", "details")})`);
			}
			if (expanded && content) {
				text += `\n${theme.fg("borderMuted", "─".repeat(36))}\n${theme.fg("toolOutput", content)}`;
			}
			return new Text(text, 0, 0);
		},
	});

	pi.registerCommand("olaunchpad", {
		description: "Open the Launchpad control center",
		getArgumentCompletions(prefix) {
			const actions = ["start", "restart", "stop", "status", "logs", "errors", "show", "launch", "troubleshoot"];
			const items = actions.filter((action) => action.startsWith(prefix)).map((action) => ({ value: action, label: action }));
			return items.length ? items : null;
		},
		handler: async (rawArgs, ctx) => {
			let [action, ...rest] = rawArgs.trim().split(/\s+/);
			if (!action) {
				if (ctx.mode !== "tui") {
					ctx.ui.notify("/olaunchpad requires an action outside interactive mode", "warning");
					return;
				}
				const menuItems: SelectItem[] = [
					{ value: "show", label: "Show Launchpad", description: "Bring the app forward and focus search" },
					{ value: "launch", label: "Launch application", description: "Search and activate the first match" },
					{ value: "logs", label: "Live logs", description: "Inspect output from the current watcher" },
					{ value: "errors", label: "Error history", description: "Inspect append-only errors.txt" },
					{ value: "restart", label: "Restart watcher", description: "Stop the process tree and start cleanly" },
					{ value: "troubleshoot", label: "Diagnostics", description: "Status, configuration, logs, and errors" },
					{ value: "stop", label: "Stop watcher", description: "Stop make dev and the app" },
				];
				action =
					(await ctx.ui.custom<string | null>(
						(tui, theme, _keybindings, done) => {
							const panel = new Container();
							panel.addChild(new DynamicBorder((text: string) => theme.fg("borderAccent", text)));
							panel.addChild(
								new Text(
									theme.fg("accent", theme.bold("  ◉ Launchpad Control Center")) +
									"\n" +
									theme.fg("dim", `  ${statusText()}`),
									0,
									0,
								),
							);
							const list = new SelectList(menuItems, menuItems.length, {
								selectedPrefix: (text) => theme.fg("accent", text),
								selectedText: (text) => theme.fg("accent", theme.bold(text)),
								description: (text) => theme.fg("muted", text),
								scrollInfo: (text) => theme.fg("dim", text),
								noMatch: (text) => theme.fg("warning", text),
							});
							list.onSelect = (item) => done(item.value);
							list.onCancel = () => done(null);
							panel.addChild(list);
							panel.addChild(new Text(theme.fg("dim", "  ↑↓ navigate  ·  enter select  ·  esc close"), 0, 0));
							panel.addChild(new DynamicBorder((text: string) => theme.fg("borderAccent", text)));
							return {
								render: (width: number) => panel.render(width),
								invalidate: () => panel.invalidate(),
								handleInput: (data: string) => {
									list.handleInput(data);
									tui.requestRender();
								},
							};
						},
						{
							overlay: true,
							overlayOptions: { width: 68, maxHeight: "80%", anchor: "center", margin: 2 },
						},
					)) ?? "";
			}
			if (!action) return;
			if (!["start", "restart", "stop", "status", "logs", "errors", "show", "launch", "troubleshoot"].includes(action)) {
				ctx.ui.notify(`Unknown olaunchpad action: ${action}`, "error");
				return;
			}
			if (action === "launch" && rest.length === 0) {
				const query = await ctx.ui.input("Launch application", "name or desktop ID");
				if (!query) return;
				rest = [query];
			}
			try {
				const output = await perform(action as Action, rest.join(" "));
				if (action === "logs" || action === "errors" || action === "troubleshoot" || action === "status") {
					if (ctx.mode === "tui") {
						await showOutputOverlay(ctx, actionLabel(action as Action), output);
					} else {
						ctx.ui.setEditorText(output);
					}
				} else {
					ctx.ui.notify(output, "info");
				}
			} catch (error) {
				ctx.ui.notify(errorMessage(error), "error");
			}
		},
	});

	pi.on("session_start", (_event, ctx) => {
		cwd = ctx.cwd;
		sessionContext = ctx;
		logPath = join(
			tmpdir(),
			`olaunchpad-shell-${createHash("sha1").update(cwd).digest("hex").slice(0, 10)}.log`,
		);
		updateUi();
	});

	pi.on("session_shutdown", async () => {
		sessionContext?.ui.setStatus(WIDGET_NAME, undefined);
		sessionContext?.ui.setWidget(WIDGET_NAME, undefined);
		sessionContext = undefined;
		await stop();
	});
}
