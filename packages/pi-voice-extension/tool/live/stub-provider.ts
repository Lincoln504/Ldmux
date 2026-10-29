export default function (pi: any) {
  pi.registerProvider("stub", { name: "Stub", baseUrl: "http://127.0.0.1:18777/v1", apiKey: "x", api: "openai-completions",
    models: [{ type: "chat", id: "stub-1", name: "Stub 1", reasoning: false, input: ["text"], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 32000, maxTokens: 2000 }] });
}
