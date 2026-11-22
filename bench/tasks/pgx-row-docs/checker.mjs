import { checkTask } from '../../checkers.mjs';
import { tasks } from '../../tasks.mjs';

const task = tasks.find(({ id }) => id === 'pgx-row-docs');
export const check = context => checkTask(task, context);
