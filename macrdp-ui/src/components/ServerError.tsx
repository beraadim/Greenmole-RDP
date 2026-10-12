export default function ServerError({ message }: { message?: string | null }) {
  if (!message) return null;
  return (
    <div role="alert" className="rounded-md border border-red/20 bg-red/10 p-3 text-xs text-red">
      <p className="mb-1 font-semibold">Service error</p>
      <p className="whitespace-pre-wrap break-words max-h-40 overflow-y-auto">{message}</p>
    </div>
  );
}
