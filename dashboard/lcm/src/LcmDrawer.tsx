/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Detail drawer of the hermes-lcm dashboard: computes the title and body for
 * the top stack entry (loading / error / node / message / session) and
 * renders the shared Drawer chrome. Extracted 1:1 from `App.tsx` — DOM
 * structure, class names, and drawer back-stack behavior unchanged.
 */

import React from "react";
import { short } from "./helpers";
import {
  Drawer,
  DrawerError,
  MessageDetail,
  NodeDetail,
  SessionDetail,
} from "./components";
import { EmptyState } from "../../lib/primitives";

interface LcmDrawerProps {
  top: any;
  canBack: boolean;
  onBack: () => void;
  onClose: () => void;
  updateStackEntry: (
    matcher: (e: any) => boolean,
    updater: (e: any) => any,
  ) => void;
  fetchNode: (id: any) => void;
  fetchMessageContext: (message: any) => void;
  fetchSession: (
    id: any,
    offset: any,
    append: boolean,
    activeMessageId: any,
  ) => void;
  onOpenNode: (id: any) => void;
  onOpenSession: (id: any, opts?: any) => void;
  onOpenMessage: (message: any) => void;
  onLoadMoreSession: (id: any) => void;
}

export function LcmDrawer({
  top,
  canBack,
  onBack,
  onClose,
  updateStackEntry,
  fetchNode,
  fetchMessageContext,
  fetchSession,
  onOpenNode,
  onOpenSession,
  onOpenMessage,
  onLoadMoreSession,
}: LcmDrawerProps): React.ReactElement {
  let drawerTitle = "";
  let drawerBody: React.ReactNode = null;
  if (top) {
    if (top.loading) {
      drawerTitle =
        top.kind === "node"
          ? `Node #${top.id}`
          : top.kind === "message"
            ? `Message #${top.id}`
            : `Session ${short(top.id, 40)}`;
      drawerBody = (
        <EmptyState className="hermes-lcm-empty">Loading…</EmptyState>
      );
    } else if (top.error) {
      drawerTitle =
        top.kind === "node"
          ? `Node #${top.id}`
          : top.kind === "message"
            ? `Message #${top.id}`
            : `Session ${short(top.id, 40)}`;
      const current = top;
      drawerBody = (
        <DrawerError
          kind={current.kind}
          message={current.error}
          onRetry={function () {
            updateStackEntry(
              function (entry) {
                return entry === current;
              },
              function (entry) {
                return Object.assign({}, entry, { loading: true, error: "" });
              },
            );
            if (current.kind === "node") fetchNode(current.id);
            else if (current.kind === "message")
              fetchMessageContext(current.data && current.data.message);
            else fetchSession(current.id, 0, false, current.activeMessageId);
          }}
        />
      );
    } else if (top.kind === "node") {
      drawerTitle = `Node #${top.id}`;
      drawerBody = (
        <NodeDetail
          data={top.data}
          onOpenNode={onOpenNode}
          onOpenSession={onOpenSession}
          onOpenMessage={onOpenMessage}
        />
      );
    } else if (top.kind === "message") {
      drawerTitle = `Message #${top.id}`;
      drawerBody = (
        <MessageDetail
          data={top.data}
          onOpenNode={onOpenNode}
          onOpenSession={onOpenSession}
        />
      );
    } else {
      drawerTitle = `Session ${short(top.id, 40)}`;
      drawerBody = (
        <SessionDetail
          data={top.data}
          onOpenNode={onOpenNode}
          onOpenMessage={onOpenMessage}
          onLoadMore={function () {
            onLoadMoreSession(top.id);
          }}
          loadingMore={!!top.loadingMore}
          activeMessageId={top.activeMessageId}
        />
      );
    }
  }

  return (
    <Drawer
      open={!!top}
      title={drawerTitle}
      canBack={canBack}
      onBack={onBack}
      onClose={onClose}
    >
      {drawerBody}
    </Drawer>
  );
}
