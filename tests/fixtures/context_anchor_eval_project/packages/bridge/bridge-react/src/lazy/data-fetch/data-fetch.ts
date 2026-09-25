export interface DataFetchParams {
  id: string;
  routeParams: Record<string, string>;
}

export async function fetchData(params: DataFetchParams): Promise<unknown> {
  return { id: params.id, routeParams: params.routeParams };
}
