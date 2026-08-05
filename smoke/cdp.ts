// Minimal Chrome DevTools Protocol client over bun's WebSocket. No npm dependencies.

type Handler = (params: any) => void;

export class Cdp {
    #ws: WebSocket;
    #next = 1;
    #pending = new Map<number, { ok: (v: any) => void; fail: (e: Error) => void }>();
    #handlers = new Map<string, Set<Handler>>();

    private constructor(ws: WebSocket) {
        this.#ws = ws;
        ws.addEventListener("message", (event) => {
            const message = JSON.parse(String((event as MessageEvent).data));
            if (message.id !== undefined) {
                const waiting = this.#pending.get(message.id);
                if (!waiting) return;
                this.#pending.delete(message.id);
                if (message.error) {
                    waiting.fail(new Error(`${message.error.message} (${message.method ?? ""})`));
                } else {
                    waiting.ok(message.result);
                }
                return;
            }
            for (const handler of this.#handlers.get(message.method) ?? []) {
                handler(message.params);
            }
        });
    }

    static connect(url: string): Promise<Cdp> {
        return new Promise((ok, fail) => {
            const ws = new WebSocket(url);
            ws.addEventListener("open", () => ok(new Cdp(ws)));
            ws.addEventListener("error", () => fail(new Error(`cannot open ${url}`)));
        });
    }

    send(method: string, params: Record<string, unknown> = {}): Promise<any> {
        const id = this.#next++;
        return new Promise((ok, fail) => {
            this.#pending.set(id, { ok, fail });
            this.#ws.send(JSON.stringify({ id, method, params }));
        });
    }

    on(event: string, handler: Handler) {
        let set = this.#handlers.get(event);
        if (!set) this.#handlers.set(event, (set = new Set()));
        set.add(handler);
    }

    /// Evaluate an expression and return its value, surfacing a thrown error as a rejection.
    async eval<T = any>(expression: string): Promise<T> {
        const result = await this.send("Runtime.evaluate", {
            expression,
            returnByValue: true,
            awaitPromise: true,
        });
        if (result.exceptionDetails) {
            const detail = result.exceptionDetails;
            throw new Error(detail.exception?.description ?? detail.text);
        }
        return result.result.value;
    }

    close() {
        this.#ws.close();
    }
}
