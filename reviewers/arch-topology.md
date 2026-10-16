# Architecture Topology

## 1. Features

Features are modules which compile with zero heavy dependencies. All heavy or outside-world dependencies must be injected as clients so that all things remain testable. If an API dependency compiles as part of a feature, you've failed. Each feature is independently buildable and runnable, with mocks or live.

## 2. Topology

One root feature coordinates between child features as siblings. One common module can represent app-local types which are shared across features. Every user-facing flow (a screen or group of screens with its own state) is its own feature module. The root feature only composes child features and owns no screens or flow logic of its own.

## 3. Injection

All outside dependencies are injected in one place at the root.
