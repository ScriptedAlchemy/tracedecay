export interface TreeshakeRequest {
  entry: string;
  exportsUsed: string[];
}

export interface TreeshakeResult {
  entry: string;
  removed: number;
}

export async function runTreeshake(request: TreeshakeRequest): Promise<TreeshakeResult> {
  return { entry: request.entry, removed: request.exportsUsed.length };
}
