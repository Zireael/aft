// Bash-output hint nudges now live in the shared bridge package so both plugin
// hosts use one implementation. Re-exported here to keep the existing OpenCode
// import path stable.
export { maybeAppendConflictsHint } from "@cortexkit/aft-bridge";
