export function Avatar({ name }: { name: string }) {
  const initial = (name || "a").slice(0, 1).toUpperCase();
  return (
    <span className="avatar" title={name}>
      {initial}
    </span>
  );
}
