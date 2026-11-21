import { tasks } from '../../tasks.mjs';
import { checkTask } from '../../checkers.mjs';
const task = tasks.find(({ id: taskId }) => taskId === 'routine-setup');
export const check = context => checkTask(task, context);
