FROM node:22.22.1-bookworm-slim
WORKDIR /app
COPY package*.json ./
COPY LICENSE.md ./
RUN npm ci --omit=dev --ignore-scripts && npm cache clean --force
COPY src ./src
COPY certificates ./certificates
USER node
ENV NODE_ENV=production PORT=8080
EXPOSE 8080
CMD ["node", "src/server.mjs"]
