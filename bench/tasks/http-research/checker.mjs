import { tasks } from '../../tasks.mjs';
import { checkTask } from '../../checkers.mjs';
const task = tasks.find(({ id: taskId }) => taskId === 'http-research');
export const check = context => checkTask(task, context);
