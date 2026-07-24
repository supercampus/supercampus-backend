# Backend Development Guidelines & Agent Rules

## Technology Stack
- **Runtime**: Node.js
- **Framework**: Express.js
- **Database**: PostgreSQL
- **ORM**: Prisma (`@prisma/client`)
- **Module System**: CommonJS (`require` / `module.exports`)

## Development & Execution Commands
- `npm run dev`: Starts the server with `nodemon` for auto-reloading during development.
- `npm start`: Starts the production server (`node src/server.js`).
- `npm run check`: Runs syntax checks on key entry points (`src/app.js` and `src/server.js`).
- `npm run prisma:generate`: Generates Prisma Client artifacts after schema changes.
- `npm run prisma:migrate`: Runs database migrations with Prisma.
- `npm run prisma:studio`: Launches Prisma Studio visual database browser.

## Code Guidelines & Standards
1. **Database & ORM Usage**:
   - Always import the shared Prisma client singleton from `src/lib/prisma.js`.
   - Keep schema models defined in `prisma/schema.prisma`.
   - Run `npm run prisma:generate` whenever `prisma/schema.prisma` is modified.
2. **Architecture & File Organization**:
   - Keep application setup/middleware in `src/app.js`.
   - Keep server setup and listener initialization in `src/server.js`.
   - Organize routes, controllers, and services in dedicated subdirectories under `src/` as the API expands.
3. **Error Handling**:
   - Always use proper HTTP status codes (`200`, `201`, `400`, `401`, `403`, `404`, `500`).
   - Standardize JSON error responses: `{ "error": "Descriptive message" }`.
4. **Environment & Configuration**:
   - Load environment variables using standard conventions (`.env`).
   - Do not hardcode secret keys or sensitive database credentials in code.

