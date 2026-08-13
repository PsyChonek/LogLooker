import { onBeforeUnmount, ref } from 'vue';
import { widthFromPointerDelta } from '@/utils/tableResize';

// Drag-to-resize for table column headers. A view (or DataTable) renders a small
// handle on the right edge of each header cell and forwards its mousedown here;
// the column's width then tracks the pointer until the button is released.
export interface ColumnResizeOptions {
  // Persist the new width for a column, in pixels
  setWidth: (key: string, width: number) => void;
  // Resize floor for a column; defaults to 48px when omitted
  minWidth?: (key: string) => number;
  // Called once when a drag ends, e.g. to write the layout to localStorage
  onEnd?: () => void;
}

export function useColumnResize(options: ColumnResizeOptions) {
  const resizingKey = ref<string | null>(null);
  let header: HTMLElement | null = null;
  let headerWasDraggable = false;
  let startX = 0;
  let startWidth = 0;

  const floor = (key: string) => options.minWidth?.(key) ?? 48;

  function onMove(event: MouseEvent) {
    const key = resizingKey.value;
    if (!key) return;
    options.setWidth(key, widthFromPointerDelta(startWidth, startX, event.clientX, floor(key)));
  }

  function stop() {
    if (!resizingKey.value) return;
    resizingKey.value = null;
    if (header && headerWasDraggable) header.draggable = true;
    header = null;
    document.removeEventListener('mousemove', onMove);
    document.removeEventListener('mouseup', stop);
    document.body.style.userSelect = '';
    document.body.style.cursor = '';
    options.onEnd?.();
  }

  // The handle lives inside the header cell; measuring the cell's rendered width
  // lets an auto-sized (widthless) column start resizing from where it actually is
  function startResize(key: string, event: MouseEvent) {
    event.preventDefault();
    event.stopPropagation();
    header = (event.target as HTMLElement).closest('th');
    if (header) {
      // A reorderable header is draggable; a native drag starting mid-resize
      // would swallow mousemove/mouseup and leave the resize stuck
      headerWasDraggable = header.draggable;
      header.draggable = false;
    }
    startWidth = header ? header.getBoundingClientRect().width : floor(key);
    startX = event.clientX;
    resizingKey.value = key;
    document.addEventListener('mousemove', onMove);
    document.addEventListener('mouseup', stop);
    document.body.style.userSelect = 'none';
    document.body.style.cursor = 'col-resize';
  }

  onBeforeUnmount(stop);

  return { resizingKey, startResize };
}
