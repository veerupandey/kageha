export function integrationError(error: unknown): string {
  const status = (error as { status?: number } | null)?.status;
  if (status === 502 || status === 503 || status === 504 || error instanceof TypeError) {
    return "Kageha’s server is unavailable. Retry when it is back online; you do not need to set up your account again.";
  }
  return error instanceof Error ? error.message : String(error);
}
