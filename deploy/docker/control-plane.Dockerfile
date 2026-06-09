# Build context: control-plane/
FROM node:22-slim AS deps
WORKDIR /app
COPY package.json ./
RUN npm install --no-audit --no-fund

FROM node:22-slim AS builder
WORKDIR /app
COPY --from=deps /app/node_modules ./node_modules
COPY . .
RUN npm run build

FROM node:22-slim AS runner
WORKDIR /app
ENV NODE_ENV=production
COPY --from=builder /app/public ./public
COPY --from=builder /app/.next/standalone ./
COPY --from=builder /app/.next/static ./.next/static
# The node:22-slim image ships a non-root `node` user (uid 1000).
USER node
EXPOSE 3000
# Use Node's built-in fetch (no extra tooling) to probe the login page, which
# renders without any backend dependency.
HEALTHCHECK --interval=30s --timeout=3s --start-period=15s --retries=3 \
    CMD node -e "fetch('http://127.0.0.1:3000/login').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"
CMD ["node", "server.js"]
