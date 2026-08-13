export interface ResizableColumn {
  resizable?: boolean;
}

// A resize grip controls the boundary on a column's right. A fixed column must
// therefore disable both its own grip and the grip immediately before it.
export function canResizeRightBoundary(
  columns: readonly ResizableColumn[],
  index: number,
): boolean {
  const column = columns[index];
  if (!column || column.resizable === false) return false;
  return columns[index + 1]?.resizable !== false;
}

// Base drag sizing on the pointer's movement from mousedown. Table columns can
// shift while their widths update, so measuring from a live cell edge creates
// feedback that lets the separator drift away from the pointer.
export function widthFromPointerDelta(
  startWidth: number,
  startX: number,
  pointerX: number,
  minWidth: number,
): number {
  return Math.round(Math.max(minWidth, startWidth + pointerX - startX));
}
