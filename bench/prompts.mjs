const TOPICS = [
  "Explain how a hash table handles collisions",
  "What are the tradeoffs of microservices vs a monolith",
  "Write a short poem about distributed systems",
  "Summarize the CAP theorem in plain language",
  "Give three tips for debugging production latency",
  "Compare REST and gRPC for internal APIs",
  "What is backpressure and why does it matter",
  "Describe how JWT authentication works",
  "List pros and cons of event-driven architecture",
  "How would you design a rate limiter",
  "Explain vector embeddings in one paragraph",
  "What causes tail latency in a gateway",
  "Describe blue-green deployments",
  "When should you use a message queue",
  "Explain idempotency for payment APIs",
];

export function randomPrompt(nonce) {
  const topic = TOPICS[Math.floor(Math.random() * TOPICS.length)];
  const variant = Math.floor(Math.random() * 10_000);
  return `${topic}. (ref ${nonce.slice(0, 8)}-${variant})`;
}
