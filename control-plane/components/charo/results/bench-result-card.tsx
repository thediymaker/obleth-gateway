export function BenchResultCard({ data }: { data: unknown }) {
  return <pre className="text-xs">{JSON.stringify(data, null, 2)}</pre>;
}
