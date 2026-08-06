/**
 * Says in the terminal title when Pi is working on a turn.
 *
 * Pi draws its working indicator on the screen, where a terminal cannot see it,
 * and writes a title that says only where the session is: `π - tuni`, start to
 * finish. Tuni decides whether a pane is busy from that title, the way it does
 * for every agent, so a Pi pane never spins, never gets marked when the turn
 * lands, and never raises a notification.
 *
 * A braille character in front of the title is what the other agents put there
 * and what tuni reads. One frame is enough: the animation is tuni's, drawn on
 * the tab, and the character is taken back off the tab name so the title reads
 * as Pi wrote it.
 *
 * Install with `pi install <path to this file>`, which writes the path into
 * `~/.pi/agent/settings.json`; `pi remove` takes it back out. Read against
 * pi-coding-agent 0.83.0.
 */

/** One braille frame, the U+2800 block tuni recognises. */
const WORKING = "⠂";

/**
 * What Pi calls the tab, rebuilt: the extension cannot read the title back, and
 * `APP_TITLE` is not among the package's exports. `π` is what Pi uses unless the
 * package was rebranded through `piConfig.name`, which the published one is not.
 */
function title(ctx) {
    const directory = ctx.cwd.split("/").filter(Boolean).pop() || ctx.cwd;
    const session = ctx.sessionManager.getSessionName();
    return session ? `π - ${session} - ${directory}` : `π - ${directory}`;
}

export default function (api) {
    // Only the interactive mode owns a terminal title; the others have no tab
    // to say anything on.
    const write = (ctx, text) => {
        if (ctx.mode === "tui") {
            ctx.ui.setTitle(text);
        }
    };

    api.on("agent_start", (_event, ctx) => write(ctx, `${WORKING} ${title(ctx)}`));
    // Settled rather than ended: Pi fires this one from a `finally`, so an
    // interrupted or failed run clears the mark as surely as a finished one,
    // and a run that is only pausing to retry does not clear it early.
    api.on("agent_settled", (_event, ctx) => write(ctx, title(ctx)));
}
