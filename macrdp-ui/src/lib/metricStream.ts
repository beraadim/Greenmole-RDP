/** Orders event/poll samples and invalidates requests across stop or unmount. */
export class MetricSampleGate {
  epoch = 0;
  private lastSampleId = 0;

  reset(): void { this.epoch++; }

  admit(sampleId: number, requestEpoch = this.epoch): boolean {
    if (requestEpoch !== this.epoch || !Number.isSafeInteger(sampleId) ||
        sampleId <= this.lastSampleId) return false;
    this.lastSampleId = sampleId;
    return true;
  }
}
